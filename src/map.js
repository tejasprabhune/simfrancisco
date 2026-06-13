// ─────────────────────────────────────────────────────────────────────────
// Canvas renderer: SF landmass + agent dots, with ambient drift, a "waiting"
// breathing state, and the staggered green/red poll reveal.
//
// Canvas (not SVG) because we animate ~1200 dots at 60fps. The frosted-glass
// UI chrome lives in the HTML overlay on top — backdrop-filter can't touch
// canvas pixels.
// ─────────────────────────────────────────────────────────────────────────

import { COLORS, TIMING } from "./config.js";
import { makeProjection, makeInside } from "./projection.js";

const POP_MS = 340; // per-dot pop duration during reveal

const easeOutBack = (x) => {
  const c1 = 1.70158, c3 = c1 + 1;
  return 1 + c3 * Math.pow(x - 1, 3) + c1 * Math.pow(x - 1, 2);
};
const clamp01 = (x) => (x < 0 ? 0 : x > 1 ? 1 : x);

export class SFMap {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.agents = [];
    this.proj = null;
    this.inside = null;
    this.mode = "idle"; // idle | waiting | reveal | results | clearing
    this.t0 = performance.now();
    this.revealT0 = 0;
    this.revealDur = TIMING.revealMs;
    this.clearT0 = 0;
    this.clearFade = 1;
    this.revealCount = 0;
    this.onProgress = null;
    this.onRevealComplete = null;
    this._raf = null;

    this._loop = this._loop.bind(this);
    this.resize = this.resize.bind(this);
    window.addEventListener("resize", this.resize);
  }

  setOutline(feature) {
    this.feature = feature;
    this.proj = makeProjection(feature);
    this.inside = makeInside(feature);
    this.resize();
  }

  // raw: [{ lonlat:[lon,lat], ... }]
  setAgents(raw) {
    const { clamp } = this.inside;
    const { toPlanar } = this.proj;
    this.agents = raw
      .filter((a) => Array.isArray(a.lonlat))
      .map((a) => {
        const [lon, lat] = clamp(a.lonlat[0], a.lonlat[1]);
        const pl = toPlanar(lon, lat);
        return {
          lon, lat,
          hx: pl.x, hy: pl.y,            // planar home (for verdict field)
          sx: 0, sy: 0,                  // screen home (set in resize)
          phase: Math.random() * Math.PI * 2,
          speed: 0.6 + Math.random() * 0.8,
          amp: 0.6 + Math.random() * 1.1,
          size: 1.7 + Math.random() * 0.9,
          verdict: null,
          activateAt: 0,
        };
      });
    this._placeScreen();
  }

  _placeScreen() {
    if (!this.proj) return;
    for (const a of this.agents) {
      const p = this.proj.project(a.lon, a.lat);
      a.sx = p.x; a.sy = p.y;
    }
  }

  resize() {
    const w = this.canvas.clientWidth || window.innerWidth;
    const h = this.canvas.clientHeight || window.innerHeight;
    this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.canvas.width = Math.round(w * this.dpr);
    this.canvas.height = Math.round(h * this.dpr);
    this.cssW = w; this.cssH = h;
    if (this.proj) {
      // Reserve the top-right zone for the summary/progress card (top:22 right:22,
      // width min(340,64vw)) on wide displays so Treasure Island never sits behind
      // the frosted card. Layout stays stable whether or not a poll is running.
      const base = 0.07;
      const cardW = Math.min(340, w * 0.64);
      const reserveRight = w >= 1024 ? cardW + 44 : w * base;
      this.proj.fit(w, h, {
        top: h * base,
        bottom: h * base,
        left: w * base,
        right: reserveRight,
      });
      this._buildLandPath();
      this._placeScreen();
    }
  }

  _buildLandPath() {
    const path = new Path2D();
    for (const ring of this.proj.rings) {
      ring.forEach(([lon, lat], i) => {
        const p = this.proj.project(lon, lat);
        if (i === 0) path.moveTo(p.x, p.y);
        else path.lineTo(p.x, p.y);
      });
      path.closePath();
    }
    this.landPath = path;
  }

  // ── state transitions ────────────────────────────────────────────────
  setWaiting() { this.mode = "waiting"; }

  startReveal(verdicts, durationMs = TIMING.revealMs) {
    this.revealDur = durationMs;
    const now = performance.now();
    this.revealT0 = now;
    this.clearFade = 1;
    const spread = durationMs * 0.82;
    this.agents.forEach((a, i) => {
      a.verdict = verdicts[i];
      a.activateAt = now + Math.random() * spread; // scattered across the map
    });
    this.revealCount = 0;
    this.mode = "reveal";
  }

  clearVerdicts() {
    if (this.mode === "idle") return;
    this.clearT0 = performance.now();
    this.mode = "clearing";
  }

  start() { if (!this._raf) this._raf = requestAnimationFrame(this._loop); }
  stop() { if (this._raf) { cancelAnimationFrame(this._raf); this._raf = null; } }

  // ── render loop ──────────────────────────────────────────────────────
  _loop(now) {
    this._raf = requestAnimationFrame(this._loop);
    this._draw(now);
  }

  _draw(now) {
    const ctx = this.ctx;
    const t = (now - this.t0) / 1000;
    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.clearRect(0, 0, this.cssW, this.cssH);

    // water — subtle vertical gradient (lighter at the top) for depth
    const wg = ctx.createLinearGradient(0, 0, 0, this.cssH);
    wg.addColorStop(0, COLORS.waterTop || COLORS.water);
    wg.addColorStop(1, COLORS.water);
    ctx.fillStyle = wg;
    ctx.fillRect(0, 0, this.cssW, this.cssH);

    if (!this.proj || !this.landPath) return;

    // landmass with soft shadow + thin coastline
    ctx.save();
    ctx.shadowColor = withAlpha(COLORS.ink, 0.18);
    ctx.shadowBlur = 28;
    ctx.shadowOffsetY = 7;
    ctx.fillStyle = COLORS.land;
    ctx.fill(this.landPath);
    ctx.restore();
    ctx.lineJoin = "round";
    ctx.lineWidth = 1.4;
    ctx.strokeStyle = withAlpha(COLORS.ink, 0.45);
    ctx.stroke(this.landPath);

    // clearing fade factor
    if (this.mode === "clearing") {
      const f = clamp01((now - this.clearT0) / TIMING.fadeBackMs);
      this.clearFade = 1 - f;
      if (f >= 1) {
        for (const a of this.agents) a.verdict = null;
        this.clearFade = 1;
        this.mode = "idle";
        this.revealCount = 0;
      }
    }

    // dots
    const breathing = this.mode === "waiting";
    let revealed = 0;
    const showVerdict = this.mode === "reveal" || this.mode === "results" || this.mode === "clearing";

    for (const a of this.agents) {
      const dx = Math.sin(t * a.speed + a.phase) * a.amp;
      const dy = Math.cos(t * a.speed * 0.9 + a.phase) * a.amp;
      const x = a.sx + dx, y = a.sy + dy;

      // base neutral dot — always present (faint), so the city never goes empty
      let baseAlpha = 0.42;
      let r = a.size;
      if (breathing) {
        const pulse = 0.5 + 0.5 * Math.sin(t * 2.2 + a.phase);
        baseAlpha = 0.32 + pulse * 0.4;
        r = a.size * (0.92 + pulse * 0.22);
      }

      let vScale = 0;
      if (showVerdict && a.verdict) {
        const local = now - a.activateAt;
        if (local >= 0) {
          revealed++;
          vScale = easeOutBack(clamp01(local / POP_MS));
        }
        vScale *= this.clearFade;
      }

      // draw neutral dot, dimming as the verdict ring takes over
      const neutralA = baseAlpha * (1 - 0.7 * clamp01(vScale));
      if (neutralA > 0.02) {
        ctx.beginPath();
        ctx.fillStyle = withAlpha(COLORS.inkSoft, neutralA);
        ctx.arc(x, y, r, 0, Math.PI * 2);
        ctx.fill();
      }

      // verdict: an outlined green/red circle
      if (vScale > 0.01) {
        const col = a.verdict === "yes" ? COLORS.yes : COLORS.no;
        const rr = (a.size + 1.6) * vScale;
        ctx.beginPath();
        ctx.fillStyle = withAlpha(col, 0.16 * clamp01(vScale));
        ctx.arc(x, y, rr, 0, Math.PI * 2);
        ctx.fill();
        ctx.beginPath();
        ctx.lineWidth = 1.5;
        ctx.strokeStyle = withAlpha(col, 0.92 * clamp01(vScale));
        ctx.arc(x, y, rr, 0, Math.PI * 2);
        ctx.stroke();
      }
    }

    // progress reporting + reveal completion
    if (this.mode === "reveal") {
      if (revealed !== this.revealCount) {
        this.revealCount = revealed;
        this.onProgress && this.onProgress(revealed, this.agents.length);
      }
      if (now > this.revealT0 + this.revealDur + POP_MS) {
        this.revealCount = this.agents.length;
        this.onProgress && this.onProgress(this.agents.length, this.agents.length);
        this.mode = "results";
        this.onRevealComplete && this.onRevealComplete();
      }
    }
  }
}

// "#rrggbb" + alpha -> rgba()
function withAlpha(hex, a) {
  const n = parseInt(hex.slice(1), 16);
  const r = (n >> 16) & 255, g = (n >> 8) & 255, b = n & 255;
  return `rgba(${r},${g},${b},${a})`;
}
