/**
 * Pure input-guard helpers for floating notification toasts so they never
 * capture keyboard focus or switch conversations on Enter/Space keystrokes.
 */

export interface PointerLikeClickEvent {
  /** Number of clicks (0 for keyboard-synthesized Enter/Space clicks, >= 1 for pointer clicks). */
  detail?: number;
  /** Mouse button index (0 = primary/left button) when present. */
  button?: number;
}

export interface KeyLikeEvent {
  key: string;
}

/**
 * Return true only for a primary-button (left) pointer press.
 */
export function isPrimaryMouseDown(e: Pick<PointerLikeClickEvent, "button">): boolean {
  return e.button === 0;
}

/**
 * Return true only for an explicit pointer click (`detail > 0`), rejecting
 * keyboard-synthesized `click` events (`detail === 0`) triggered by Enter/Space.
 */
export function isExplicitPointerClick(e: PointerLikeClickEvent): boolean {
  return typeof e.detail === "number" && e.detail > 0;
}

/**
 * Decide how the floating notification popup window should react to a keydown
 * event if focus ever unexpectedly reaches it:
 * - `"dismiss"` on Escape
 * - `"block"` on Enter / Space (explicitly swallowed; never opens a thread)
 * - `"ignore"` for all other keys
 */
export function classifyNotifyPopupKey(e: KeyLikeEvent): "dismiss" | "block" | "ignore" {
  if (e.key === "Escape") {
    return "dismiss";
  }
  if (e.key === "Enter" || e.key === " " || e.key === "Spacebar") {
    return "block";
  }
  return "ignore";
}
