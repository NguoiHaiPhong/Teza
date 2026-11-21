import type { PasscodeConfig } from "../types";

export const PASSCODE_KEY = "litechat.passcode.v1";

export const DEFAULT_PASSCODE_CONFIG: PasscodeConfig = {
  enabled: false,
  salt: "",
  hash: "",
  autoLockMinutes: 5,
};

const PBKDF2_ITERATIONS = 120_000;

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
    throw new Error("Salt không hợp lệ");
  }
  const buf = new ArrayBuffer(clean.length / 2);
  const out = new Uint8Array(buf);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = Number.parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

export function loadPasscodeConfig(): PasscodeConfig {
  try {
    if (typeof localStorage === "undefined") return { ...DEFAULT_PASSCODE_CONFIG };
    const raw = localStorage.getItem(PASSCODE_KEY);
    if (!raw) return { ...DEFAULT_PASSCODE_CONFIG };
    const parsed = JSON.parse(raw) as Partial<PasscodeConfig>;
    return {
      enabled: Boolean(parsed.enabled && parsed.salt && parsed.hash),
      salt: typeof parsed.salt === "string" ? parsed.salt : "",
      hash: typeof parsed.hash === "string" ? parsed.hash : "",
      autoLockMinutes:
        typeof parsed.autoLockMinutes === "number" && parsed.autoLockMinutes >= 0
          ? parsed.autoLockMinutes
          : DEFAULT_PASSCODE_CONFIG.autoLockMinutes,
    };
  } catch {
    return { ...DEFAULT_PASSCODE_CONFIG };
  }
}

export function savePasscodeConfig(cfg: PasscodeConfig): void {
  try {
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(PASSCODE_KEY, JSON.stringify(cfg));
    }
  } catch {
    // ignore storage quota failures
  }
}

export function validatePasscodeCandidate(pin: string, confirmPin?: string): string | null {
  if (pin.length < 4) {
    return "Mã khóa phải có ít nhất 4 ký tự";
  }
  if (confirmPin !== undefined && pin !== confirmPin) {
    return "Mã khóa xác nhận chưa khớp";
  }
  return null;
}

export async function hashPasscode(
  pin: string,
  saltHex?: string,
): Promise<{ salt: string; hash: string }> {
  const subtle = globalThis.crypto?.subtle;
  if (!subtle) {
    throw new Error("Trình duyệt không hỗ trợ WebCrypto PBKDF2");
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
