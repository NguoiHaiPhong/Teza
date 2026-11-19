import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { isTauri } from "../bridge";

let lastSoundAt = 0;

/** Synthesize a soft two-note incoming message chime via Web Audio API. */
export function playMessageSound(): void {
  const nowMs = Date.now();
  if (nowMs - lastSoundAt < 900) return;
  lastSoundAt = nowMs;

  try {
    const AudioCtx =
      window.AudioContext ||
      (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
    if (!AudioCtx) return;
    const ctx = new AudioCtx();
    const now = ctx.currentTime;

    const notes: [number, number, number][] = [
      [0, 0.1, 587.33],
      [0.09, 0.14, 880],
    ];
    for (const [offset, dur, freq] of notes) {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.setValueAtTime(freq, now + offset);
      gain.gain.setValueAtTime(0.001, now + offset);
      gain.gain.exponentialRampToValueAtTime(0.065, now + offset + 0.015);
      gain.gain.exponentialRampToValueAtTime(0.001, now + offset + dur);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now + offset);
      osc.stop(now + offset + dur + 0.01);
    }

    setTimeout(() => {
      void ctx.close().catch(() => undefined);
    }, 400);
  } catch {
    // Ignore autoplay restrictions
  }
}

export async function showDesktopNotification(title: string, body: string): Promise<void> {
  const safeTitle = title.trim() || "litechat";
  const safeBody = body.trim().slice(0, 200);
  if (!safeBody) return;

  if (isTauri()) {
    try {
      let granted = await isPermissionGranted();
      if (!granted) {
        granted = (await requestPermission()) === "granted";
      }
      if (granted) {
        sendNotification({ title: safeTitle, body: safeBody });
      }
    } catch {
      // Ignore notification errors silently
    }
    return;
  }

  if (typeof Notification !== "undefined" && Notification.permission === "granted") {
    new Notification(safeTitle, { body: safeBody });
  }
}
