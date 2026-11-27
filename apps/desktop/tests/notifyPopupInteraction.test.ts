import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import {
  classifyNotifyPopupKey,
  isExplicitPointerClick,
  isPrimaryMouseDown,
} from "../src/components/notifyPopupInteraction.ts";

const __dirname = dirname(fileURLToPath(import.meta.url));

test("isPrimaryMouseDown only accepts left mouse button presses", () => {
  assert.equal(isPrimaryMouseDown({ button: 0 }), true);
  assert.equal(isPrimaryMouseDown({ button: 1 }), false);
  assert.equal(isPrimaryMouseDown({ button: 2 }), false);
  assert.equal(isPrimaryMouseDown({}), false);
});

test("isExplicitPointerClick accepts real pointer clicks and rejects keyboard-synthesized clicks", () => {
  assert.equal(isExplicitPointerClick({ detail: 1 }), true);
  assert.equal(isExplicitPointerClick({ detail: 2 }), true);
  // Keyboard Enter/Space activation dispatches click with detail === 0
  assert.equal(isExplicitPointerClick({ detail: 0 }), false);
  assert.equal(isExplicitPointerClick({}), false);
});

test("classifyNotifyPopupKey blocks Enter/Space and only dismisses on Escape", () => {
  assert.equal(classifyNotifyPopupKey({ key: "Enter" }), "block");
  assert.equal(classifyNotifyPopupKey({ key: " " }), "block");
  assert.equal(classifyNotifyPopupKey({ key: "Spacebar" }), "block");
  assert.equal(classifyNotifyPopupKey({ key: "Escape" }), "dismiss");
  assert.equal(classifyNotifyPopupKey({ key: "a" }), "ignore");
  assert.equal(classifyNotifyPopupKey({ key: "Tab" }), "ignore");
});

test("NotifyToastWindow keeps all controls non-focusable and never opens on Enter", () => {
  const src = readFileSync(
    join(__dirname, "../src/components/NotifyToastWindow.tsx"),
    "utf8",
  );
  assert.equal(
    src.includes("tabIndex={0}"),
    false,
    "NotifyToastWindow must not use tabIndex={0}",
  );
  assert.equal(
    src.includes("tabIndex={-1}"),
    true,
    "NotifyToastWindow interactive elements must explicitly set tabIndex={-1}",
  );
  assert.equal(
    src.includes("isExplicitPointerClick"),
    true,
    "NotifyToastWindow click handlers must guard with isExplicitPointerClick",
  );
  assert.equal(
    src.includes("classifyNotifyPopupKey"),
    true,
    "NotifyToastWindow keydown listener must guard with classifyNotifyPopupKey",
  );
});
