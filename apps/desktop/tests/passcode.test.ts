import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_PASSCODE_CONFIG,
  PASSCODE_BASE_LOCKOUT_MS,
  PASSCODE_KEY,
  PASSCODE_LOCKOUT_THRESHOLD,
  PASSCODE_MAX_LOCKOUT_MS,
  computePasscodeLockoutMs,
  getPasscodeLockoutRemainingMs,
  hashPasscode,
  loadPasscodeConfig,
  recordPasscodeFailure,
  resetPasscodeLockout,
  savePasscodeConfig,
  validatePasscodeCandidate,
  validatePasscodeCandidateCode,
  verifyPasscode,
} from "../src/utils/passcode.ts";

function installMemoryLocalStorage() {
  const store = new Map<string, string>();
  const mockStorage = {
    getItem: (key: string) => (store.has(key) ? store.get(key)! : null),
    setItem: (key: string, value: string) => {
      store.set(key, String(value));
    },
    removeItem: (key: string) => {
      store.delete(key);
    },
    clear: () => {
      store.clear();
    },
    key: (idx: number) => Array.from(store.keys())[idx] ?? null,
    get length() {
      return store.size;
    },
  };
  const prev = (globalThis as { localStorage?: Storage }).localStorage;
  Object.defineProperty(globalThis, "localStorage", {
    value: mockStorage,
    configurable: true,
    writable: true,
  });
  return () => {
    if (prev === undefined) {
      delete (globalThis as { localStorage?: Storage }).localStorage;
    } else {
      Object.defineProperty(globalThis, "localStorage", {
        value: prev,
        configurable: true,
        writable: true,
      });
    }
  };
}

test("hashPasscode and verifyPasscode verify correct PIN and reject wrong PIN", async () => {
  const { salt, hash } = await hashPasscode("2580");
  assert.equal(salt.length, 32);
  assert.equal(hash.length, 64);

  const cfg = {
    ...DEFAULT_PASSCODE_CONFIG,
    enabled: true,
    salt,
    hash,
  };

  assert.equal(await verifyPasscode("2580", cfg), true);
  assert.equal(await verifyPasscode("2581", cfg), false);
  assert.equal(await verifyPasscode("", cfg), false);
  assert.equal(await verifyPasscode("2580", { ...cfg, enabled: false }), false);
});

test("hashPasscode uses random salt unless specified and rejects corrupted salt cleanly", async () => {
  const first = await hashPasscode("9999");
  const second = await hashPasscode("9999");
  assert.notEqual(first.salt, second.salt);
  assert.notEqual(first.hash, second.hash);

  assert.equal(
    await verifyPasscode("9999", {
      enabled: true,
      salt: "not-hex",
      hash: first.hash,
    }),
    false,
  );
});

test("validatePasscodeCandidate supports codes, explicit languages, stored UI language, and custom translator", () => {
  assert.equal(validatePasscodeCandidateCode("123"), "too_short");
  assert.equal(validatePasscodeCandidateCode("1234", "1235"), "mismatch");
  assert.equal(validatePasscodeCandidateCode("1234", "1234"), null);

  assert.ok(validatePasscodeCandidate("123"));
  assert.ok(validatePasscodeCandidate("1234", "1235"));
  assert.equal(validatePasscodeCandidate("1234", "1234"), null);

  assert.equal(validatePasscodeCandidate("123", undefined, "en"), "Passcode must be at least 4 characters");
  assert.equal(validatePasscodeCandidate("1234", "9999", "en"), "Confirmation passcode does not match");
  assert.equal(validatePasscodeCandidate("123", undefined, "ko"), "비밀번호는 최소 4자 이상이어야 합니다");
  assert.equal(validatePasscodeCandidate("1234", "9999", "ko"), "확인 비밀번호가 일치하지 않습니다");

  assert.equal(
    validatePasscodeCandidate("123", undefined, (key) => `tr:${key}`),
    "tr:passcodeErrTooShort",
  );
  assert.equal(
    validatePasscodeCandidate("1234", "9999", (key) => `tr:${key}`),
    "tr:passcodeErrMismatch",
  );

  const restore = installMemoryLocalStorage();
  try {
    localStorage.setItem("teza.uiLanguage", "en");
    assert.equal(validatePasscodeCandidate("123"), "Passcode must be at least 4 characters");
    localStorage.setItem("teza.uiLanguage", "ko");
    assert.equal(validatePasscodeCandidate("1234", "4321"), "확인 비밀번호가 일치하지 않습니다");
  } finally {
    restore();
  }
});

test("progressive exponential lockout persists across reloads and resets on unlock", async () => {
  assert.equal(computePasscodeLockoutMs(0), 0);
  assert.equal(computePasscodeLockoutMs(PASSCODE_LOCKOUT_THRESHOLD - 1), 0);
  assert.equal(computePasscodeLockoutMs(PASSCODE_LOCKOUT_THRESHOLD), PASSCODE_BASE_LOCKOUT_MS);
  assert.equal(computePasscodeLockoutMs(PASSCODE_LOCKOUT_THRESHOLD + 1), PASSCODE_BASE_LOCKOUT_MS * 2);
  assert.equal(computePasscodeLockoutMs(PASSCODE_LOCKOUT_THRESHOLD + 2), PASSCODE_BASE_LOCKOUT_MS * 4);
  assert.equal(computePasscodeLockoutMs(30), PASSCODE_MAX_LOCKOUT_MS);

  const restore = installMemoryLocalStorage();
  try {
    const { salt, hash } = await hashPasscode("2580");
    savePasscodeConfig({
      ...DEFAULT_PASSCODE_CONFIG,
      enabled: true,
      salt,
      hash,
    });

    const t0 = 1_700_000_000_000;
    const f1 = recordPasscodeFailure(undefined, t0);
    assert.equal(f1.failedAttempts, 1);
    assert.equal(f1.lockoutUntilMs, 0);

    const f2 = recordPasscodeFailure(undefined, t0 + 1000);
    assert.equal(f2.failedAttempts, 2);
    assert.equal(f2.lockoutUntilMs, 0);

    const f3 = recordPasscodeFailure(undefined, t0 + 2000);
    assert.equal(f3.failedAttempts, 3);
    assert.equal(f3.lockoutUntilMs, t0 + 2000 + PASSCODE_BASE_LOCKOUT_MS);

    // Reload from localStorage verifies persistence
    const reloaded = loadPasscodeConfig();
    assert.equal(reloaded.failedAttempts, 3);
    assert.equal(reloaded.lockoutUntilMs, t0 + 2000 + PASSCODE_BASE_LOCKOUT_MS);
    assert.equal(getPasscodeLockoutRemainingMs(reloaded, t0 + 7000), PASSCODE_BASE_LOCKOUT_MS - 5000);
    assert.equal(getPasscodeLockoutRemainingMs(reloaded, t0 + 2000 + PASSCODE_BASE_LOCKOUT_MS + 1), 0);

    // 4th failure doubles cooldown
    const f4 = recordPasscodeFailure(undefined, t0 + 20_000);
    assert.equal(f4.failedAttempts, 4);
    assert.equal(f4.lockoutUntilMs, t0 + 20_000 + PASSCODE_BASE_LOCKOUT_MS * 2);

    // Clock skew protection clamps absurd future lockoutUntilMs
    assert.equal(
      getPasscodeLockoutRemainingMs({ lockoutUntilMs: t0 + 365 * 24 * 3600_000 }, t0),
      PASSCODE_MAX_LOCKOUT_MS,
    );

    // Reset clears both counters and persists
    const reset = resetPasscodeLockout();
    assert.equal(reset.failedAttempts, 0);
    assert.equal(reset.lockoutUntilMs, 0);
    const reloadedAfterReset = loadPasscodeConfig();
    assert.equal(reloadedAfterReset.failedAttempts, 0);
    assert.equal(reloadedAfterReset.lockoutUntilMs, 0);
    assert.ok(localStorage.getItem(PASSCODE_KEY));
  } finally {
    restore();
  }
});
