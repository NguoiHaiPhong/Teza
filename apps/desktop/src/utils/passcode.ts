import type { UiLanguage } from "../i18n/types";
import type { PasscodeConfig } from "../types";

export const PASSCODE_KEY = "litechat.passcode.v1";

const UI_LANG_STORAGE_KEY = "teza.uiLanguage";

export const PASSCODE_LOCKOUT_THRESHOLD = 3;
export const PASSCODE_BASE_LOCKOUT_MS = 15_000;
export const PASSCODE_MAX_LOCKOUT_MS = 15 * 60_000;

export interface PasscodeLockoutFields {
  failedAttempts?: number;
  lockoutUntilMs?: number;
}

export type PasscodeConfigWithLockout = PasscodeConfig & PasscodeLockoutFields;

export type PasscodeValidationErrorCode = "too_short" | "mismatch";
export type PasscodeErrorTranslationKey = "passcodeErrTooShort" | "passcodeErrMismatch";

const PASSCODE_VALIDATION_MESSAGES: Record<
  UiLanguage,
  Record<PasscodeValidationErrorCode, string>
> = {
  vi: {
    too_short: "Mã khóa phải có ít nhất 4 ký tự",
    mismatch: "Mã khóa xác nhận chưa khớp",
  },
  en: {
    too_short: "Passcode must be at least 4 characters",
    mismatch: "Confirmation passcode does not match",
  },
  ko: {
    too_short: "비밀번호는 최소 4자 이상이어야 합니다",
    mismatch: "확인 비밀번호가 일치하지 않습니다",
  },
};

export const DEFAULT_PASSCODE_CONFIG: PasscodeConfigWithLockout = {
  enabled: false,
  salt: "",
  hash: "",
  autoLockMinutes: 5,
  failedAttempts: 0,
  lockoutUntilMs: 0,
};

const PBKDF2_ITERATIONS = 120_000;

function readStoredUiLanguage(): UiLanguage {
  try {
    if (typeof localStorage === "undefined") return "vi";
    const raw = localStorage.getItem(UI_LANG_STORAGE_KEY);
    return raw === "en" || raw === "ko" ? raw : "vi";
  } catch {
    return "vi";
  }
}

function sanitizeNonNegInt(raw: unknown): number {
  if (typeof raw !== "number" || !Number.isFinite(raw) || raw <= 0) return 0;
  return Math.floor(raw);
}

function sanitizeNonNegMs(raw: unknown): number {
  if (typeof raw !== "number" || !Number.isFinite(raw) || raw <= 0) return 0;
  return Math.floor(raw);
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 1) {
    out += bytes[i].toString(16).padStart(2, "0");
  }
  return out;
}

function hexToBytes(hex: string): Uint8Array<ArrayBuffer> {
  const clean = hex.trim();
  if (clean.length === 0 || clean.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(clean)) {
    throw new Error("Invalid passcode salt");
  }
  const buf = new ArrayBuffer(clean.length / 2);
  const out = new Uint8Array(buf);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

export function loadPasscodeConfig(): PasscodeConfigWithLockout {
  try {
    if (typeof localStorage === "undefined") return { ...DEFAULT_PASSCODE_CONFIG };
    const raw = localStorage.getItem(PASSCODE_KEY);
    if (!raw) return { ...DEFAULT_PASSCODE_CONFIG };
    const parsed = JSON.parse(raw) as Partial<PasscodeConfigWithLockout>;
    const enabled = Boolean(parsed.enabled && parsed.salt && parsed.hash);
    return {
      enabled,
      salt: typeof parsed.salt === "string" ? parsed.salt : "",
      hash: typeof parsed.hash === "string" ? parsed.hash : "",
      autoLockMinutes:
        typeof parsed.autoLockMinutes === "number" &&
        Number.isFinite(parsed.autoLockMinutes) &&
        parsed.autoLockMinutes >= 0
          ? parsed.autoLockMinutes
          : DEFAULT_PASSCODE_CONFIG.autoLockMinutes,
      failedAttempts: enabled ? sanitizeNonNegInt(parsed.failedAttempts) : 0,
      lockoutUntilMs: enabled ? sanitizeNonNegMs(parsed.lockoutUntilMs) : 0,
    };
  } catch {
    return { ...DEFAULT_PASSCODE_CONFIG };
  }
}

export function savePasscodeConfig(cfg: PasscodeConfigWithLockout): void {
  try {
    if (typeof localStorage !== "undefined") {
      const enabled = Boolean(cfg.enabled && cfg.salt && cfg.hash);
      const normalized: PasscodeConfigWithLockout = {
        ...cfg,
        failedAttempts: enabled ? sanitizeNonNegInt(cfg.failedAttempts) : 0,
        lockoutUntilMs: enabled ? sanitizeNonNegMs(cfg.lockoutUntilMs) : 0,
      };
      localStorage.setItem(PASSCODE_KEY, JSON.stringify(normalized));
    }
  } catch {
    // ignore storage quota failures
  }
}

export function computePasscodeLockoutMs(failedAttempts: number): number {
  const count = sanitizeNonNegInt(failedAttempts);
  if (count < PASSCODE_LOCKOUT_THRESHOLD) {
    return 0;
  }
  const exponent = Math.min(20, count - PASSCODE_LOCKOUT_THRESHOLD);
  return Math.min(PASSCODE_MAX_LOCKOUT_MS, PASSCODE_BASE_LOCKOUT_MS * 2 ** exponent);
}

export function getPasscodeLockoutRemainingMs(
  cfg: Pick<PasscodeConfigWithLockout, "lockoutUntilMs">,
  nowMs = Date.now(),
): number {
  const until = sanitizeNonNegMs(cfg.lockoutUntilMs);
  if (until <= nowMs) return 0;
  return Math.min(PASSCODE_MAX_LOCKOUT_MS, until - nowMs);
}

export function recordPasscodeFailure(
  cfg?: PasscodeConfigWithLockout,
  nowMs = Date.now(),
): PasscodeConfigWithLockout {
  const base = cfg ?? loadPasscodeConfig();
  const failedAttempts = sanitizeNonNegInt(base.failedAttempts) + 1;
  const cooldownMs = computePasscodeLockoutMs(failedAttempts);
  const lockoutUntilMs = cooldownMs > 0 ? nowMs + cooldownMs : 0;
  const next: PasscodeConfigWithLockout = {
    ...base,
    failedAttempts,
    lockoutUntilMs,
  };
  savePasscodeConfig(next);
  return next;
}

export function resetPasscodeLockout(
  cfg?: PasscodeConfigWithLockout,
): PasscodeConfigWithLockout {
  const base = cfg ?? loadPasscodeConfig();
  const next: PasscodeConfigWithLockout = {
    ...base,
    failedAttempts: 0,
    lockoutUntilMs: 0,
  };
  savePasscodeConfig(next);
  return next;
}

export function validatePasscodeCandidateCode(
  pin: string,
  confirmPin?: string,
): PasscodeValidationErrorCode | null {
  if (pin.length < 4) {
    return "too_short";
  }
  if (confirmPin !== undefined && pin !== confirmPin) {
    return "mismatch";
  }
  return null;
}

export function validatePasscodeCandidate(
  pin: string,
  confirmPin?: string,
  langOrTranslator?: UiLanguage | ((key: PasscodeErrorTranslationKey) => string),
): string | null {
  const code = validatePasscodeCandidateCode(pin, confirmPin);
  if (!code) return null;
  if (typeof langOrTranslator === "function") {
    return langOrTranslator(
      code === "too_short" ? "passcodeErrTooShort" : "passcodeErrMismatch",
    );
  }
  const lang: UiLanguage =
    langOrTranslator === "en" || langOrTranslator === "ko" || langOrTranslator === "vi"
      ? langOrTranslator
      : readStoredUiLanguage();
  return PASSCODE_VALIDATION_MESSAGES[lang][code];
}

export async function hashPasscode(
  pin: string,
  saltHex?: string,
): Promise<{ salt: string; hash: string }> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    throw new Error("WebCrypto PBKDF2 is unavailable");
  }
  let saltBytes: Uint8Array<ArrayBuffer>;
  let salt: string;
  if (saltHex) {
    saltBytes = hexToBytes(saltHex);
    salt = saltHex;
  } else {
    const saltBuf = new ArrayBuffer(16);
    saltBytes = new Uint8Array(saltBuf);
    globalThis.crypto.getRandomValues(saltBytes);
    salt = bytesToHex(saltBytes);
  }
  const pinBytes = new TextEncoder().encode(pin);
  const key = await subtle.importKey("raw", pinBytes, { name: "PBKDF2" }, false, [
    "deriveBits",
  ]);
  const bits = await subtle.deriveBits(
    {
      name: "PBKDF2",
      hash: "SHA-256",
      salt: saltBytes,
      iterations: PBKDF2_ITERATIONS,
    },
    key,
    256,
  );
  return {
    salt,
    hash: bytesToHex(new Uint8Array(bits)),
  };
}

export async function verifyPasscode(
  pin: string,
  cfg: Pick<PasscodeConfig, "enabled" | "salt" | "hash">,
): Promise<boolean> {
  if (!cfg.enabled || !cfg.salt || !cfg.hash || !pin) {
    return false;
  }
  try {
    const { hash } = await hashPasscode(pin, cfg.salt);
    if (hash.length !== cfg.hash.length) return false;
    let diff = 0;
    for (let i = 0; i < hash.length; i += 1) {
      diff |= hash.charCodeAt(i) ^ cfg.hash.charCodeAt(i);
    }
    return diff === 0;
  } catch {
    return false;
  }
}
