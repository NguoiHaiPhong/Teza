import { useEffect, useRef } from "react";

interface Props {
  enabled: boolean;
  dustEnabled?: boolean;
}

const PRESS_SELECTOR = 'a[href], button, summary, [role="button"], .tf-row';
const BLUE_RGB = "150,215,255";
const GOLD_RGB = "255,212,120";
const TORCH_HALF = 260;

function createFairyWingBody(variant: 1 | 2, rgb: string): HTMLDivElement {
  const wrap = document.createElement("div");
  wrap.className = `b b${variant}`;
  wrap.style.setProperty("--c", rgb);

  const glow = document.createElement("i");
  glow.className = "glow";

  const svgNs = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(svgNs, "svg");
  svg.setAttribute("viewBox", "-42 -34 84 64");
  svg.setAttribute("width", "84");
  svg.setAttribute("height", "64");

  const defs = document.createElementNS(svgNs, "defs");
  const grad = document.createElementNS(svgNs, "linearGradient");
  const gradId = `fw-${variant}`;
  grad.setAttribute("id", gradId);
  grad.setAttribute("gradientUnits", "userSpaceOnUse");
  grad.setAttribute("x1", "0");
  grad.setAttribute("y1", "0");
  grad.setAttribute("x2", "36");
  grad.setAttribute("y2", "0");

  for (const [offset, color, opacity] of [
    ["0", "#fff", ".95"],
    [".5", `rgb(${rgb})`, ".6"],
    ["1", `rgb(${rgb})`, ".1"],
  ] as const) {
    const stop = document.createElementNS(svgNs, "stop");
    stop.setAttribute("offset", offset);
    stop.setAttribute("stop-color", color);
    stop.setAttribute("stop-opacity", opacity);
    grad.appendChild(stop);
  }
  defs.appendChild(grad);
  svg.appendChild(defs);

  for (const flip of [false, true]) {
    const side = document.createElementNS(svgNs, "g");
    if (flip) side.setAttribute("transform", "scale(-1 1)");

    const upper = document.createElementNS(svgNs, "g");
    upper.setAttribute("class", "flap");
    const upperPath = document.createElementNS(svgNs, "path");
    upperPath.setAttribute("d", "M2 -1C8 -14 22 -27 36 -28C37 -17 24 -5 2 -1Z");
    upperPath.setAttribute("fill", `url(#${gradId})`);
    upper.appendChild(upperPath);

    const lower = document.createElementNS(svgNs, "g");
    lower.setAttribute("class", "flap lo");
    const lowerPath = document.createElementNS(svgNs, "path");
    lowerPath.setAttribute("d", "M2 1C9 3 20 11 25 21C15 19 6 10 2 1Z");
    lowerPath.setAttribute("fill", `url(#${gradId})`);
    lower.appendChild(lowerPath);

    side.append(upper, lower);
    svg.appendChild(side);
  }

  const glint = document.createElement("i");
  glint.className = "glint";
  const core = document.createElement("i");
  core.className = "core";

  wrap.append(glow, svg, glint, core);
  return wrap;
}

export function TriforceEffects({ enabled, dustEnabled }: Props) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const torchRef = useRef<HTMLDivElement | null>(null);
  const activeDust = dustEnabled ?? enabled;

  useEffect(() => {
    const prefersReduced =
      typeof window !== "undefined" &&
      window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    const active = enabled && !prefersReduced;
    const runDust = active && activeDust;
    document.body.classList.toggle("fx-off", !active);

    if (!active) {
      document.body.classList.remove("lit");
      return;
    }

    // 1. Fairy & sparkle trail (GPU transform-only, auto-sleeps when idle)
    const fay = document.createElement("div");
    fay.id = "triforceFay";
    fay.className = "fay";
    fay.setAttribute("aria-hidden", "true");
    fay.append(createFairyWingBody(1, BLUE_RGB), createFairyWingBody(2, GOLD_RGB));

    const trail = document.createElement("div");
    trail.className = "fay-trail";
    trail.setAttribute("aria-hidden", "true");
    document.body.append(trail, fay);

    let px = 0;
    let py = 0;
    let x = 0;
    let y = 0;
    let vx = 0;
    let vy = 0;
    let on = false;
    let running = false;
    let litApplied = false;
    let lastFairy = 0;
    let moved = 0;
    let lastHitTestAt = 0;
    let hot: Element | null = null;
    let hotRect: DOMRect | null = null;
    let sparkAt = 0;
    let alive = 0;
    let fairyRaf = 0;
    let torchRaf = 0;
    let torchX = 0;
    let torchY = 0;
    let visible = !document.hidden;

    const spark = (sx: number, sy: number, burst: boolean) => {
      if (alive >= 14) return;
      const s = document.createElement("i");
      const d = 2 + Math.random() * 2.5;
      const a = Math.random() * 6.283;
      const r = burst ? 16 + Math.random() * 24 : 3 + Math.random() * 7;
      s.style.cssText =
        `left:${sx.toFixed(1)}px;top:${sy.toFixed(1)}px;` +
        `width:${d.toFixed(1)}px;height:${d.toFixed(1)}px;` +
        `--dx:${(Math.cos(a) * r).toFixed(1)}px;` +
        `--dy:${(Math.sin(a) * r + (burst ? 0 : 12 + Math.random() * 14)).toFixed(1)}px;` +
        `--t:${(0.5 + Math.random() * 0.45).toFixed(2)}s;` +
        `--c:${hot ? GOLD_RGB : BLUE_RGB}`;
      alive += 1;
      s.addEventListener(
        "animationend",
        () => {
          s.remove();
          alive = Math.max(0, alive - 1);
        },
        { once: true },
      );
      trail.appendChild(s);
    };

    const aim = (target: Element | null) => {
      if (target !== hot) {
        hot = target;
        hotRect = target && target.isConnected ? target.getBoundingClientRect() : null;
        fay.classList.toggle("hot", !!target);
      }
    };

    const stepFairy = (t: number) => {
      if (!on || !visible) {
        running = false;
        return;
      }
      const dt = lastFairy ? Math.min(0.034, (t - lastFairy) / 1000) : 0.016;
      lastFairy = t;
      const idleMs = t - moved;
      const a = t * 0.0021;
      let gx: number;
      let gy: number;

      if (hot && hot.isConnected && hotRect) {
        gx = Math.max(hotRect.left + 6, Math.min(hotRect.right - 6, px));
        gy = hotRect.top < 40 ? hotRect.bottom + 13 : hotRect.top - 13;
      } else {
        gx = px + 26 + (idleMs < 1800 ? Math.cos(a * 1.3) * 4 : 0);
        gy = py - 28 + (idleMs < 1800 ? Math.sin(a * 2.1) * 4 : 0);
      }

      vx += ((gx - x) * 120 - vx * 15) * dt;
      vy += ((gy - y) * 120 - vy * 15) * dt;
      x += vx * dt;
      y += vy * dt;

      fay.style.transform = `translate3d(${x.toFixed(1)}px,${y.toFixed(1)}px,0) rotate(${Math.max(
        -24,
        Math.min(24, vx * 0.035),
      ).toFixed(1)}deg)`;

      const speed = Math.hypot(vx, vy);
      if (speed > 170 && t - sparkAt > 80) {
        sparkAt = t;
        spark(x + (Math.random() - 0.5) * 8, y + (Math.random() - 0.5) * 8, false);
      }

      // Stop rAF completely once settled after mouse stops moving (0% idle CPU/GPU)
      if (idleMs > 2200 && speed < 8 && Math.hypot(gx - x, gy - y) < 3) {
        running = false;
        lastFairy = 0;
        return;
      }
      fairyRaf = requestAnimationFrame(stepFairy);
    };

    const flushTorch = () => {
      torchRaf = 0;
      const torchEl = torchRef.current;
      if (torchEl) {
        torchEl.style.transform = `translate3d(${(torchX - TORCH_HALF).toFixed(1)}px,${(
          torchY - TORCH_HALF
        ).toFixed(1)}px,0)`;
      }
    };

    const onPointerMove = (e: PointerEvent) => {
      if (!litApplied) {
        litApplied = true;
        document.body.classList.add("lit");
      }
      if (torchRef.current) {
        torchX = e.clientX;
        torchY = e.clientY;
        if (!torchRaf) {
          torchRaf = requestAnimationFrame(flushTorch);
        }
      }

      if (e.pointerType && e.pointerType !== "mouse") return;
      px = e.clientX;
      py = e.clientY;
      const now = performance.now();
      moved = now;

      if (now - lastHitTestAt >= 50) {
        lastHitTestAt = now;
        const target = e.target instanceof Element ? e.target.closest(PRESS_SELECTOR) : null;
        aim(target);
      }

      if (!on) {
        on = true;
        x = px - 90;
        y = py - 70;
        vx = 0;
        vy = 0;
        lastFairy = 0;
        fay.classList.add("on");
      }
      if (!running && visible) {
        running = true;
        lastFairy = 0;
        fairyRaf = requestAnimationFrame(stepFairy);
      }
    };

    const onPointerDown = (e: PointerEvent) => {
      if (on && (!e.pointerType || e.pointerType === "mouse")) {
        for (let i = 0; i < 5; i += 1) {
          spark(x, y, true);
        }
      }
    };

    const onMouseLeave = () => {
      on = false;
      running = false;
      fay.classList.remove("on");
    };

    const invalidateHotRect = () => {
      if (hot && hot.isConnected) {
        hotRect = hot.getBoundingClientRect();
      } else {
        hotRect = null;
      }
    };

    window.addEventListener("pointermove", onPointerMove, { passive: true });
    window.addEventListener("pointerdown", onPointerDown, { passive: true });
    window.addEventListener("scroll", invalidateHotRect, { passive: true, capture: true });
    document.documentElement.addEventListener("mouseleave", onMouseLeave);

    // 2. Floating ambient gold dust on canvas (paused during active typing & hidden tab)
    const canvas = canvasRef.current;
    const ctx = runDust ? canvas?.getContext("2d") ?? null : null;
    let w = 0;
    let h = 0;
    let lastDust = 0;
    let typingUntil = 0;
    let dustRaf = 0;

    const count = Math.max(14, Math.min(26, Math.round(window.innerWidth / 56)));
    const particles = Array.from({ length: count }, () => ({
      x: Math.random(),
      y: Math.random(),
      z: 0.25 + Math.random() * 0.75,
      r: 0.7 + Math.random() * 1.9,
      a: 0.08 + Math.random() * 0.24,
      v: 0.006 + Math.random() * 0.014,
      s: Math.random() * 6.283,
    }));

    const fit = () => {
      if (!canvas) return;
      w = canvas.width = Math.max(1, Math.round(window.innerWidth));
      h = canvas.height = Math.max(1, Math.round(window.innerHeight));
      invalidateHotRect();
    };
    fit();

    const sprite = document.createElement("canvas");
    sprite.width = 32;
    sprite.height = 32;
    const sCtx = sprite.getContext("2d");
    if (sCtx) {
      const grad = sCtx.createRadialGradient(16, 16, 0, 16, 16, 16);
      grad.addColorStop(0, "rgba(255,244,204,1)");
      grad.addColorStop(0.35, "rgba(247,220,140,0.45)");
      grad.addColorStop(1, "rgba(247,220,140,0)");
      sCtx.fillStyle = grad;
      sCtx.fillRect(0, 0, 32, 32);
    }

    const onTypingKey = (e: KeyboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "TEXTAREA" || target.tagName === "INPUT")) {
        typingUntil = performance.now() + 900;
      }
    };
    if (runDust) {
      window.addEventListener("keydown", onTypingKey, { passive: true });
    }

    const MIN_FRAME_MS = 60;
    const stepDust = (t: number) => {
      if (!ctx || !visible) return;
      if (t < typingUntil || (lastDust && t - lastDust < MIN_FRAME_MS)) {
        dustRaf = requestAnimationFrame(stepDust);
        return;
      }
      const dt = lastDust ? Math.min(90, t - lastDust) : 60;
      lastDust = t;
      ctx.clearRect(0, 0, w, h);
      for (const p of particles) {
        p.y -= (p.v * p.z * dt) / 1000;
        if (p.y < -0.05) {
          p.y = 1.05;
          p.x = Math.random();
        }
        const gx = (p.x + Math.sin(t * 0.00022 + p.s) * 0.012 * p.z) * w;
        const gy = p.y * h;
        const rad = p.r * p.z * 1.4;
        const size = rad * 6.0;
        const alpha = p.a * (0.55 + 0.45 * Math.sin(t * 0.0011 + p.s * 3));
        ctx.globalAlpha = alpha;
        ctx.drawImage(sprite, gx - size * 0.5, gy - size * 0.5, size, size);
      }
      ctx.globalAlpha = 1;
      dustRaf = requestAnimationFrame(stepDust);
    };

    const onVisibilityChange = () => {
      visible = !document.hidden;
      if (!visible) {
        running = false;
        cancelAnimationFrame(fairyRaf);
        cancelAnimationFrame(dustRaf);
        return;
      }
      if (ctx) {
        lastDust = 0;
        dustRaf = requestAnimationFrame(stepDust);
      }
    };

    window.addEventListener("resize", fit);
    document.addEventListener("visibilitychange", onVisibilityChange);
    if (ctx) {
      dustRaf = requestAnimationFrame(stepDust);
    } else if (canvas) {
      canvas.getContext("2d")?.clearRect(0, 0, canvas.width, canvas.height);
    }

    return () => {
      on = false;
      running = false;
      cancelAnimationFrame(fairyRaf);
      cancelAnimationFrame(dustRaf);
      cancelAnimationFrame(torchRaf);
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerdown", onPointerDown);
      window.removeEventListener("scroll", invalidateHotRect, true);
      if (runDust) {
        window.removeEventListener("keydown", onTypingKey);
      }
      document.documentElement.removeEventListener("mouseleave", onMouseLeave);
      window.removeEventListener("resize", fit);
      document.removeEventListener("visibilitychange", onVisibilityChange);
      fay.remove();
      trail.remove();
      if (ctx && canvas) {
        ctx.clearRect(0, 0, canvas.width, canvas.height);
      }
    };
  }, [enabled, activeDust]);

  return (
    <>
      {activeDust && <canvas ref={canvasRef} id="dust" aria-hidden="true" />}
      {activeDust && <div ref={torchRef} className="torch" aria-hidden="true" />}
    </>
  );
}
