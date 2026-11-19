import type { MessageDto } from "../types";

export const BUZZ_TEXT = "⚡ BUZZ!";
export const BUZZ_COOLDOWN_MS = 5_000;

export function isBuzzMessage(msg: MessageDto): boolean {
  if (msg.is_recalled) return false;
  const trimmed = msg.text.trim();
  return trimmed === BUZZ_TEXT || /^⚡\s*BUZZ!?$/i.test(trimmed);
}

/** Synthesize a punchy Yahoo-classic style BUZZ alert using Web Audio API (no external audio assets). */
export function playBuzzSound(): void {
  try {
    const AudioCtx =
      window.AudioContext ||
      (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!AudioCtx) return;
    const ctx = new AudioCtx();
    const now = ctx.currentTime;

    const bursts: [number, number, number][] = [
      [0, 0.16, 190],
      [0.2, 0.16, 175],
      [0.4, 0.3, 150],
    ];
    for (const [offset, dur, freq] of bursts) {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sawtooth";
      osc.frequency.setValueAtTime(freq, now + offset);
      osc.frequency.linearRampToValueAtTime(freq * 1.18, now + offset + dur * 0.45);
      osc.frequency.linearRampToValueAtTime(freq * 0.88, now + offset + dur);

      gain.gain.setValueAtTime(0.001, now + offset);
      gain.gain.exponentialRampToValueAtTime(0.18, now + offset + 0.02);
      gain.gain.exponentialRampToValueAtTime(0.001, now + offset + dur);

      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now + offset);
      osc.stop(now + offset + dur + 0.01);
    }

    setTimeout(() => {
      void ctx.close().catch(() => undefined);
    }, 900);
  } catch {
    // Ignore autoplay or WebAudio restrictions gracefully
  }
}
