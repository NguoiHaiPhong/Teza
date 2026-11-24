function getAudioContextCtor(): typeof AudioContext | null {
  if (typeof window === "undefined") return null;
  return (
    window.AudioContext ||
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext ||
    null
  );
}

/** Play a soft, non-intrusive repeating incoming ringtone via Web Audio API. */
export function startIncomingRingtone(enabled: boolean): () => void {
  if (!enabled || typeof window === "undefined") {
    return () => undefined;
  }
  const AudioCtx = getAudioContextCtor();
  if (!AudioCtx) {
    return () => undefined;
  }

  let closed = false;
  let ctx: AudioContext | null = null;
  let timer: number | null = null;

  const playBurst = () => {
    if (closed) return;
    try {
      if (!ctx || ctx.state === "closed") {
        ctx = new AudioCtx();
      }
      if (ctx.state === "suspended") {
        void ctx.resume().catch(() => undefined);
      }
      const now = ctx.currentTime;
      const notes: [number, number, number][] = [
        [0, 0.22, 659.25],
        [0.24, 0.28, 880],
        [0.58, 0.22, 659.25],
        [0.82, 0.3, 880],
      ];
      for (const [offset, dur, freq] of notes) {
        const osc = ctx.createOscillator();
        const gain = ctx.createGain();
        osc.type = "sine";
        osc.frequency.setValueAtTime(freq, now + offset);
        gain.gain.setValueAtTime(0.001, now + offset);
        gain.gain.exponentialRampToValueAtTime(0.055, now + offset + 0.02);
        gain.gain.exponentialRampToValueAtTime(0.001, now + offset + dur);
        osc.connect(gain);
        gain.connect(ctx.destination);
        osc.start(now + offset);
        osc.stop(now + offset + dur + 0.015);
      }
    } catch {
      // Ignore WebAudio autoplay restrictions gracefully
    }
  };

  playBurst();
  timer = window.setInterval(playBurst, 2600);

  return () => {
    closed = true;
    if (timer !== null) {
      window.clearInterval(timer);
      timer = null;
    }
    if (ctx) {
      const toClose = ctx;
      ctx = null;
      void toClose.close().catch(() => undefined);
    }
  };
}

/** Play a subtle brief connect or hangup cue via Web Audio API. */
export function playCallCueTone(kind: "connect" | "end", enabled: boolean): void {
  if (!enabled || typeof window === "undefined") return;
  const AudioCtx = getAudioContextCtor();
  if (!AudioCtx) return;

  try {
    const ctx = new AudioCtx();
    const now = ctx.currentTime;
    const notes: [number, number, number][] =
      kind === "connect"
        ? [
            [0, 0.08, 523.25],
            [0.08, 0.12, 783.99],
          ]
        : [
            [0, 0.08, 493.88],
            [0.09, 0.14, 349.23],
          ];
    for (const [offset, dur, freq] of notes) {
      const osc = ctx.createOscillator();
      const gain = ctx.createGain();
      osc.type = "sine";
      osc.frequency.setValueAtTime(freq, now + offset);
      gain.gain.setValueAtTime(0.001, now + offset);
      gain.gain.exponentialRampToValueAtTime(0.05, now + offset + 0.015);
      gain.gain.exponentialRampToValueAtTime(0.001, now + offset + dur);
      osc.connect(gain);
      gain.connect(ctx.destination);
      osc.start(now + offset);
      osc.stop(now + offset + dur + 0.015);
    }
    window.setTimeout(() => {
      void ctx.close().catch(() => undefined);
    }, 450);
  } catch {
    // Ignore autoplay errors silently
  }
}
