import type { MessageDto } from "../types.ts";

export const BUZZ_TEXT = "⚡ BUZZ!";
export const BUZZ_COOLDOWN_MS = 5_000;
export const BUZZ_STREAK_WINDOW_MS = 120_000;
export const BUZZ_FULL_SCREEN_THRESHOLD = 3;

export interface BuzzStreakEntry {
  count: number;
  lastAt: number;
  seenMsgIds: string[];
}

export type BuzzStreakMap = Map<string, BuzzStreakEntry>;

export function recordIncomingBuzz(
  map: BuzzStreakMap,
  threadId: string,
  msgId: string,
  nowMs = Date.now(),
): { count: number; isDuplicate: boolean; shouldShakeScreen: boolean } {
  const prev = map.get(threadId);
  if (msgId && prev?.seenMsgIds.includes(msgId)) {
    return {
      count: prev.count,
      isDuplicate: true,
      shouldShakeScreen: false,
    };
  }

  const withinWindow = prev !== undefined && nowMs - prev.lastAt <= BUZZ_STREAK_WINDOW_MS;
  const count = withinWindow ? prev.count + 1 : 1;
  const seenBase = withinWindow ? prev.seenMsgIds : [];
  const seenMsgIds = msgId ? [...seenBase.slice(-9), msgId] : seenBase;
  map.set(threadId, { count, lastAt: nowMs, seenMsgIds });

  return {
    count,
    isDuplicate: false,
    shouldShakeScreen: count >= BUZZ_FULL_SCREEN_THRESHOLD,
  };
}

export function resetBuzzStreak(map: BuzzStreakMap, threadId: string): void {
  map.delete(threadId);
}

export function isBuzzMessage(msg: Pick<MessageDto, "is_recalled" | "text">): boolean {
  if (msg.is_recalled) return false;
  const trimmed = msg.text.trim();
  return trimmed === BUZZ_TEXT || /^⚡\s*BUZZ!?$/i.test(trimmed);
}

/** Synthesize a punchy Yahoo-classic style BUZZ alert using Web Audio API (no external audio assets). */
export function playBuzzSound(intense = false): void {
  try {
    const AudioCtx =
      window.AudioContext ||
      (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!AudioCtx) return;
    const ctx = new AudioCtx();
    const now = ctx.currentTime;

    const bursts: [number, number, number][] = intense
      ? [
          [0, 0.14, 215],
          [0.16, 0.14, 200],
          [0.32, 0.16, 185],
          [0.52, 0.34, 155],
        ]
      : [
          [0, 0.16, 190],
          [0.2, 0.16, 175],
          [0.4, 0.3, 150],
        ];
    const peakGain = intense ? 0.22 : 0.18;

    for (const [offset, dur, freq] of bursts) {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sawtooth";
      osc.frequency.setValueAtTime(freq, now + offset);
      osc.frequency.linearRampToValueAtTime(freq * 1.18, now + offset + dur * 0.45);
      osc.frequency.linearRampToValueAtTime(freq * 0.88, now + offset + dur);

      gain.gain.setValueAtTime(0.001, now + offset);
      gain.gain.exponentialRampToValueAtTime(peakGain, now + offset + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.001, now + offset + dur);

      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now + offset);
      osc.stop(now + offset + dur + 0.01);
    }

    setTimeout(() => {
      void ctx.close().catch(() => undefined);
    }, 1100);
  } catch {
    // Ignore autoplay or WebAudio restrictions gracefully
  }
}
