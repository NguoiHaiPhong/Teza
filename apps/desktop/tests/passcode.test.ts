import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_PASSCODE_CONFIG,
  hashPasscode,
  validatePasscodeCandidate,
  verifyPasscode,
} from "../src/utils/passcode.ts";

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

test("validatePasscodeCandidate enforces minimum length and matching confirmation", () => {
  assert.ok(validatePasscodeCandidate("123"));
  assert.ok(validatePasscodeCandidate("1234", "1235"));
  assert.equal(validatePasscodeCandidate("1234", "1234"), null);
});
