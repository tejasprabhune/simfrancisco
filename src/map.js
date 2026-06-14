// ─────────────────────────────────────────────────────────────────────────
// Pixel-art city renderer: the golden-future-map whole-city tile image as the
// base, with 16×16 RPG character sprites walking on top (replacing the dots).
//
// Camera model: world space == base-image pixels. The camera animates between
// "fit the whole city" (overview) and "zoomed into a clicked spot" (detail,
// fidelity enough to see sprites moving on the roads). A Return button restores
// the overview. Only the visible source sub-rect of the base image is blitted
// each frame, and off-screen sprites are culled, so it stays fast at any zoom.
//
// The frosted UI chrome lives in the HTML overlay; backdrop-filter can't touch
// canvas pixels.
// ─────────────────────────────────────────────────────────────────────────

import { COLORS, TIMING, MAP } from "./config.js";

const POP_MS = 340; // per-sprite verdict pop duration

// sprite sheet 01-generic: 240×128 = 10 chars in 5×2 blocks of 48×64.
// block c: bx=(c%5)*48, by=floor(c/5)*64. Within a block: 3 cols (frames) × 4
// rows (facings). row: down0 left1 right2 up3. col: stepA0 idle1 stepB2. cell 16.
const SHEET = { blockW: 48, blockH: 64, cell: 16, perRow: 5, nChars: 10 };
const WALK = [1, 0, 1, 2];      // contact-pass-contact-pass
const WALK_FPS = 8;
const SPRITE_WORLD = 22;        // sprite footprint in world px (~one 2 m cell ×~)

// Pokémon-style thought bubbles (shown when zoomed in)
const BUBBLE = { maxAtOnce: 7, sep: 165, cycleMs: 2600, font: '600 11px "neue-haas-grotesk-display", -apple-system, sans-serif', maxW: 150 };
// short persona thoughts keyed by the agent's dominant issue salience (real backend value vector)
const THOUGHTS = {
  s_housing:     ["rent is brutal here", "we need more housing", "another rent hike…", "priced out again"],
  s_crime:       ["is it safe to walk home?", "another car break-in", "tired of the break-ins", "where are the cops?"],
  s_homeless:    ["the city has to help folks outside", "so many tents lately", "this isn't working"],
  s_cost:        ["everything's so expensive", "groceries cost a fortune", "two jobs and still broke"],
  s_environment: ["gotta bike more", "spare-the-air day", "love these foggy mornings", "save the coast"],
  s_immigration: ["thinking of family back home", "finally getting my papers", "new to the city"],
};
const easeOutBack = (x) => { const c1 = 1.70158, c3 = c1 + 1; return 1 + c3 * Math.pow(x - 1, 3) + c1 * Math.pow(x - 1, 2); };

function ambientThought(values, id) {
  if (!values) return "just another day in the city";
  const keys = ["s_housing", "s_crime", "s_homeless", "s_cost", "s_environment", "s_immigration"];
  let best = keys[0], bv = -Infinity;
  for (const k of keys) { const v = values[k] ?? 0; if (v > bv) { bv = v; best = k; } }
  const arr = THOUGHTS[best];
  return arr[(id >>> 0) % arr.length];
}
const clamp01 = (x) => (x < 0 ? 0 : x > 1 ? 1 : x);
const lerp = (a, b, t) => a + (b - a) * t;

export class SFMap {
  constructor(canvas) {
    this.canvas = canvas;
    this.ctx = canvas.getContext("2d");
    this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.agents = [];

    this.imgW = 2144; this.imgH = 1920;           // updated when base loads
    this.base = new Image(); this.baseReady = false; this.landMask = null;
    this.base.onload = () => {
      this.imgW = this.base.naturalWidth; this.imgH = this.base.naturalHeight;
      this.baseReady = true; this._fitOverview(true); this._buildLandMask();
    };
    this.base.src = MAP.base;
    this.sprite = new Image(); this.spriteReady = false;
    this.sprite.onload = () => { this.spriteReady = true; };
    this.sprite.src = MAP.sprites;

    this.cam = { x: 1072, y: 960, zoom: 0.4 };
    this.camTarget = { ...this.cam };
    this.zoomedIn = false;
    this.onZoomChange = null;                      // (zoomedIn:boolean) => {}

    this.mode = "idle";                            // idle|waiting|reveal|results|clearing
    this.t0 = performance.now();
    this.lastT = this.t0;
    this.revealT0 = 0; this.revealDur = TIMING.revealMs;
    this.clearT0 = 0; this.clearFade = 1; this.revealCount = 0;
    this.onProgress = null; this.onRevealComplete = null;
    this.bubbleIdx = []; this.bubbleT = 0;   // which sprites currently show a thought bubble
    this._raf = null;

    this._loop = this._loop.bind(this);
    this.resize = this.resize.bind(this);
    window.addEventListener("resize", this.resize);
    canvas.addEventListener("click", (e) => this._onClick(e));
    this.resize();
  }

  // legacy hook from the old vector map — the pixel base replaces the outline.
  setOutline() {}

  // shims used by app.js: verdict field sizing + offline fallback scatter.
  get proj() {
    return {
      planarSize: { w: this.imgW, h: this.imgH },
      bbox: { minLon: MAP.bbox.west, maxLon: MAP.bbox.east, minLat: MAP.bbox.south, maxLat: MAP.bbox.north },
    };
  }
  get inside() { return { inside: () => true, clamp: (lon, lat) => [lon, lat] }; }

  lonlatToWorld(lon, lat) {
    const b = MAP.bbox;
    return {
      x: ((lon - b.west) / (b.east - b.west)) * this.imgW,
      y: ((b.north - lat) / (b.north - b.south)) * this.imgH,
    };
  }

  // raw: [{ lonlat:[lon,lat], ... }]
  setAgents(raw) {
    this.agents = raw
      .filter((a) => Array.isArray(a.lonlat))
      .map((a, i) => {
        const w0 = this.lonlatToWorld(a.lonlat[0], a.lonlat[1]);
        const w = this._snapToLand(w0.x, w0.y);   // keep every resident on land
        const ang = Math.random() * Math.PI * 2;
        return {
          wx: w.x, wy: w.y,
          hx: w.x, hy: w.y,                 // verdict-field home (world px)
          char: i % SHEET.nChars,
          dir: 0, frame: 1, frameClock: Math.random() * 1000,
          ang, speed: 5 + Math.random() * 9, // world px/sec
          turnClock: Math.random() * 2.5,
          verdict: null, activateAt: 0,
          thought: ambientThought(a.values, a.id ?? i),   // persona thought (backend value vector)
          rationale: null,                                  // set from a poll's sample_rationales
        };
      });
  }

  // Build a land/water mask from the base image so sprites never wander into the
  // bay/ocean. Water in sf_tiles.png is deep blue rgb(33,92,129) (+ wave variants);
  // everything else (grass, roads, buildings, sand) is land. Same-origin image, so
  // getImageData is allowed.
  _buildLandMask() {
    try {
      const oc = document.createElement("canvas");
      oc.width = this.imgW; oc.height = this.imgH;
      const octx = oc.getContext("2d", { willReadFrequently: true });
      octx.imageSmoothingEnabled = false;
      octx.drawImage(this.base, 0, 0);
      const data = octx.getImageData(0, 0, this.imgW, this.imgH).data;
      const mask = new Uint8Array(this.imgW * this.imgH);
      for (let i = 0, p = 0; i < mask.length; i++, p += 4) {
        const r = data[p], g = data[p + 1], b = data[p + 2];
        const water = b > 100 && b - r > 28 && b - g > 12;   // blue-dominant
        mask[i] = water ? 0 : 1;                              // 1 = land
      }
      this.landMask = mask;
    } catch (e) {
      console.warn("land mask unavailable:", e);
      this.landMask = null;
    }
    // snap any already-placed agents onto land
    for (const a of this.agents) {
      const s = this._snapToLand(a.wx, a.wy);
      a.wx = s.x; a.wy = s.y; a.hx = s.x; a.hy = s.y;
    }
  }

  _isLand(wx, wy) {
    if (!this.landMask) return true;
    const x = wx | 0, y = wy | 0;
    if (x < 0 || y < 0 || x >= this.imgW || y >= this.imgH) return false;
    return this.landMask[y * this.imgW + x] === 1;
  }

  // nearest land pixel via an outward spiral (most agents are already on land)
  _snapToLand(wx, wy) {
    if (this._isLand(wx, wy)) return { x: wx, y: wy };
    for (let r = 2; r <= 260; r += 2) {
      for (let a = 0; a < 16; a++) {
        const ang = (a / 16) * Math.PI * 2;
        const nx = wx + Math.cos(ang) * r, ny = wy + Math.sin(ang) * r;
        if (this._isLand(nx, ny)) return { x: nx, y: ny };
      }
    }
    return { x: this.imgW / 2, y: this.imgH / 2 };
  }

  // ── camera ───────────────────────────────────────────────────────────────
  _fitZoom() { return Math.min(this.cssW / this.imgW, this.cssH / this.imgH); }

  _fitOverview(snap) {
    const z = this._fitZoom();
    this.camTarget = { x: this.imgW / 2, y: this.imgH / 2, zoom: z };
    this.zoomedIn = false;
    if (snap) this.cam = { ...this.camTarget };
    this.onZoomChange && this.onZoomChange(false);
  }

  zoomTo(wx, wy) {
    const z = this._fitZoom() * MAP.detailZoomMul;
    this.camTarget = this._clamped({ x: wx, y: wy, zoom: z });
    this.zoomedIn = true;
    this.onZoomChange && this.onZoomChange(true);
  }

  returnToOverview() { this._fitOverview(false); }

  // keep the camera centre such that the viewport stays over the image
  _clamped(c) {
    const halfW = (this.cssW / 2) / c.zoom;
    const halfH = (this.cssH / 2) / c.zoom;
    let x = c.x, y = c.y;
    if (this.imgW * c.zoom > this.cssW) x = Math.max(halfW, Math.min(this.imgW - halfW, x));
    else x = this.imgW / 2;
    if (this.imgH * c.zoom > this.cssH) y = Math.max(halfH, Math.min(this.imgH - halfH, y));
    else y = this.imgH / 2;
    return { x, y, zoom: c.zoom };
  }

  screenToWorld(sx, sy) {
    return { x: this.cam.x + (sx - this.cssW / 2) / this.cam.zoom, y: this.cam.y + (sy - this.cssH / 2) / this.cam.zoom };
  }
  worldToScreen(wx, wy) {
    return { x: (wx - this.cam.x) * this.cam.zoom + this.cssW / 2, y: (wy - this.cam.y) * this.cam.zoom + this.cssH / 2 };
  }

  _onClick(e) {
    const r = this.canvas.getBoundingClientRect();
    const w = this.screenToWorld(e.clientX - r.left, e.clientY - r.top);
    // clicking re-centres at detail zoom (lets you walk the city); the Return
    // button (app.js) takes you back to the whole-city overview.
    this.zoomTo(w.x, w.y);
  }

  resize() {
    const w = this.canvas.clientWidth || window.innerWidth;
    const h = this.canvas.clientHeight || window.innerHeight;
    this.dpr = Math.min(window.devicePixelRatio || 1, 2);
    this.canvas.width = Math.round(w * this.dpr);
    this.canvas.height = Math.round(h * this.dpr);
    this.cssW = w; this.cssH = h;
    if (!this.zoomedIn) this._fitOverview(true);
    else this.camTarget = this._clamped(this.camTarget);
  }

  // ── state transitions (same surface app.js drives) ─────────────────────────
  setWaiting() { this.mode = "waiting"; }

  startReveal(verdicts, durationMs = TIMING.revealMs) {
    this.revealDur = durationMs;
    const now = performance.now();
    this.revealT0 = now; this.clearFade = 1;
    const spread = durationMs * 0.82;
    this.agents.forEach((a, i) => { a.verdict = verdicts[i]; a.activateAt = now + Math.random() * spread; });
    this.revealCount = 0; this.mode = "reveal";
  }

  clearVerdicts() {
    for (const a of this.agents) a.rationale = null;
    if (this.mode === "idle") return;
    this.clearT0 = performance.now(); this.mode = "clearing";
  }

  // distribute a poll's real sample_rationales across agents as thought bubbles
  setRationales(rationales) {
    const arr = (rationales || []).filter(Boolean);
    if (!arr.length) return;
    for (let i = 0; i < this.agents.length; i++) this.agents[i].rationale = arr[i % arr.length];
  }

  start() { if (!this._raf) { this.lastT = performance.now(); this._raf = requestAnimationFrame(this._loop); } }
  stop() { if (this._raf) { cancelAnimationFrame(this._raf); this._raf = null; } }

  // ── render loop ───────────────────────────────────────────────────────────
  _loop(now) { this._raf = requestAnimationFrame(this._loop); this._draw(now); }

  _draw(now) {
    const ctx = this.ctx;
    const dt = Math.min(0.05, (now - this.lastT) / 1000);
    this.lastT = now;

    // animate camera toward target (snappy critically-damped-ish lerp)
    const k = 1 - Math.pow(0.0001, dt); // ~time-constant independent of fps
    this.cam.x = lerp(this.cam.x, this.camTarget.x, k);
    this.cam.y = lerp(this.cam.y, this.camTarget.y, k);
    this.cam.zoom = lerp(this.cam.zoom, this.camTarget.zoom, k);

    ctx.setTransform(this.dpr, 0, 0, this.dpr, 0, 0);
    ctx.fillStyle = COLORS.water;
    ctx.fillRect(0, 0, this.cssW, this.cssH);

    // base map: blit only the visible source sub-rect, scaled (crisp pixels)
    if (this.baseReady) {
      const z = this.cam.zoom;
      let l = this.cam.x - (this.cssW / 2) / z, t = this.cam.y - (this.cssH / 2) / z;
      let r = this.cam.x + (this.cssW / 2) / z, b = this.cam.y + (this.cssH / 2) / z;
      l = Math.max(0, l); t = Math.max(0, t); r = Math.min(this.imgW, r); b = Math.min(this.imgH, b);
      if (r > l && b > t) {
        const d = this.worldToScreen(l, t);
        ctx.imageSmoothingEnabled = false;
        ctx.drawImage(this.base, l, t, r - l, b - t, d.x, d.y, (r - l) * z, (b - t) * z);
      }
    }

    // clearing fade factor
    if (this.mode === "clearing") {
      const f = clamp01((now - this.clearT0) / TIMING.fadeBackMs);
      this.clearFade = 1 - f;
      if (f >= 1) { for (const a of this.agents) a.verdict = null; this.clearFade = 1; this.mode = "idle"; this.revealCount = 0; }
    }

    this._drawSprites(now, dt);
    this._updateBubbles(now);
    this._drawBubbles();

    // reveal progress + completion (unchanged contract for app.js)
    if (this.mode === "reveal") {
      let revealed = 0;
      for (const a of this.agents) if (a.verdict && now >= a.activateAt) revealed++;
      if (revealed !== this.revealCount) { this.revealCount = revealed; this.onProgress && this.onProgress(revealed, this.agents.length); }
      if (now > this.revealT0 + this.revealDur + POP_MS) {
        this.revealCount = this.agents.length;
        this.onProgress && this.onProgress(this.agents.length, this.agents.length);
        this.mode = "results";
        this.onRevealComplete && this.onRevealComplete();
      }
    }
  }

  _drawSprites(now, dt) {
    const ctx = this.ctx;
    const z = this.cam.zoom;
    const drawPx = Math.max(3, SPRITE_WORLD * z);          // on-screen sprite height
    const showVerdict = this.mode === "reveal" || this.mode === "results" || this.mode === "clearing";
    const breathing = this.mode === "waiting";
    const margin = drawPx * 2;
    const spriteOk = this.spriteReady;

    for (const a of this.agents) {
      // wander: occasionally pick a new heading; integrate but never step into water
      a.turnClock -= dt;
      if (a.turnClock <= 0) { a.ang += (Math.random() - 0.5) * 1.6; a.turnClock = 1.2 + Math.random() * 2.4; }
      const vx = Math.cos(a.ang) * a.speed, vy = Math.sin(a.ang) * a.speed;
      const nx = a.wx + vx * dt, ny = a.wy + vy * dt;
      if (this._isLand(nx, ny)) { a.wx = nx; a.wy = ny; }
      else { a.ang += Math.PI * 0.65 + (Math.random() - 0.5) * 0.8; a.turnClock = 0.25; } // turn off the shoreline
      // facing from velocity (down0 left1 right2 up3)
      a.dir = Math.abs(vx) > Math.abs(vy) ? (vx < 0 ? 1 : 2) : (vy < 0 ? 3 : 0);
      // walk frame cycle
      a.frameClock += dt * 1000;
      const fi = Math.floor(a.frameClock / (1000 / WALK_FPS)) % WALK.length;
      a.frame = WALK[fi];

      const s = this.worldToScreen(a.wx, a.wy);
      if (s.x < -margin || s.x > this.cssW + margin || s.y < -margin || s.y > this.cssH + margin) continue;

      // verdict pop scale
      let vScale = 0;
      if (showVerdict && a.verdict) {
        const local = now - a.activateAt;
        if (local >= 0) vScale = easeOutBack(clamp01(local / POP_MS)) * this.clearFade;
      }

      const w = drawPx, h = drawPx;
      const footX = s.x, footY = s.y;     // feet anchored to the cell

      if (spriteOk) {
        const bx = (a.char % SHEET.perRow) * SHEET.blockW;
        const by = Math.floor(a.char / SHEET.perRow) * SHEET.blockH;
        const sx = bx + a.frame * SHEET.cell;
        const sy = by + a.dir * SHEET.cell;
        ctx.imageSmoothingEnabled = false;
        ctx.globalAlpha = breathing ? 0.55 + 0.35 * Math.sin(now / 240 + a.wx) : 1;
        ctx.drawImage(this.sprite, sx, sy, SHEET.cell, SHEET.cell, footX - w / 2, footY - h, w, h);
        ctx.globalAlpha = 1;
      } else {
        // sprite sheet not ready yet → neutral dot so the city is never empty
        ctx.beginPath(); ctx.fillStyle = withAlpha(COLORS.inkSoft, 0.6);
        ctx.arc(footX, footY - h / 2, Math.max(1.5, w * 0.18), 0, Math.PI * 2); ctx.fill();
      }

      // verdict marker: a colored dot floating just above the sprite's head
      if (vScale > 0.01) {
        const col = a.verdict === "yes" ? COLORS.yes : COLORS.no;
        const rr = Math.max(2.2, w * 0.22) * clamp01(vScale);
        const mx = footX, my = footY - h - rr * 0.6;
        ctx.beginPath(); ctx.fillStyle = withAlpha(col, 0.92 * clamp01(vScale));
        ctx.arc(mx, my, rr, 0, Math.PI * 2); ctx.fill();
        ctx.beginPath(); ctx.strokeStyle = withAlpha("#ffffff", 0.85 * clamp01(vScale)); ctx.lineWidth = Math.max(1, rr * 0.25);
        ctx.arc(mx, my, rr, 0, Math.PI * 2); ctx.stroke();
      }
    }
  }

  // Pick a rotating, spread-apart set of on-screen sprites to show thought bubbles
  // (only when zoomed in, so the overview stays clean and fast).
  _updateBubbles(now) {
    if (!this.zoomedIn) { this.bubbleIdx = []; return; }
    if (now < this.bubbleT && this.bubbleIdx.length) return;
    this.bubbleT = now + BUBBLE.cycleMs;
    const cand = [];
    for (let i = 0; i < this.agents.length; i++) {
      const a = this.agents[i];
      const s = this.worldToScreen(a.wx, a.wy);
      if (s.x < 60 || s.x > this.cssW - 60 || s.y < 140 || s.y > this.cssH - 80) continue;
      cand.push({ i, x: s.x, y: s.y });
    }
    // shuffle so the selection rotates each cycle, then greedily pick spread-apart ones
    for (let k = cand.length - 1; k > 0; k--) { const j = (Math.random() * (k + 1)) | 0; [cand[k], cand[j]] = [cand[j], cand[k]]; }
    const picked = [];
    for (const c of cand) {
      if (picked.length >= BUBBLE.maxAtOnce) break;
      if (picked.every((p) => Math.hypot(p.x - c.x, p.y - c.y) > BUBBLE.sep)) picked.push(c);
    }
    this.bubbleIdx = picked.map((p) => p.i);
  }

  _drawBubbles() {
    if (!this.bubbleIdx.length) return;
    const ctx = this.ctx;
    const showRat = this.mode === "reveal" || this.mode === "results";
    const drawPx = Math.max(3, SPRITE_WORLD * this.cam.zoom);
    for (const i of this.bubbleIdx) {
      const a = this.agents[i];
      if (!a) continue;
      const text = (showRat && a.rationale) ? a.rationale : a.thought;
      if (!text) continue;
      const s = this.worldToScreen(a.wx, a.wy);
      this._drawBubble(ctx, s.x, s.y - drawPx - 5, text);
    }
  }

  // a small Pokémon-style speech box (white fill, black border, downward tail)
  _drawBubble(ctx, cx, tipY, text) {
    ctx.font = BUBBLE.font;
    const pad = 6, lh = 14, tail = 7, radius = 5;
    // word-wrap to BUBBLE.maxW, max 3 lines
    const words = String(text).split(/\s+/);
    const lines = []; let line = "";
    for (const w of words) {
      const t = line ? line + " " + w : w;
      if (ctx.measureText(t).width > BUBBLE.maxW && line) { lines.push(line); line = w; }
      else line = t;
      if (lines.length >= 3) break;
    }
    if (line && lines.length < 3) lines.push(line);
    if (!lines.length) return;
    let bw = 0; for (const l of lines) bw = Math.max(bw, ctx.measureText(l).width);
    bw = Math.ceil(bw) + pad * 2;
    const bh = lines.length * lh + pad * 2;
    let bx = Math.round(cx - bw / 2);
    bx = Math.max(6, Math.min(this.cssW - bw - 6, bx));
    const by = Math.round(tipY - tail - bh);

    ctx.save();
    ctx.lineJoin = "round";
    // box
    ctx.beginPath();
    roundRect(ctx, bx, by, bw, bh, radius);
    ctx.fillStyle = "#ffffff"; ctx.fill();
    ctx.lineWidth = 2; ctx.strokeStyle = "#141414"; ctx.stroke();
    // tail (anchored toward the sprite, clamped within the box width)
    const tcx = Math.max(bx + 10, Math.min(bx + bw - 10, Math.round(cx)));
    ctx.beginPath();
    ctx.moveTo(tcx - 6, by + bh - 1);
    ctx.lineTo(tcx + 6, by + bh - 1);
    ctx.lineTo(tcx, by + bh + tail);
    ctx.closePath();
    ctx.fillStyle = "#ffffff"; ctx.fill();
    ctx.beginPath();            // re-stroke only the two slanted edges so the box base stays open
    ctx.moveTo(tcx - 6, by + bh - 1); ctx.lineTo(tcx, by + bh + tail); ctx.lineTo(tcx + 6, by + bh - 1);
    ctx.lineWidth = 2; ctx.strokeStyle = "#141414"; ctx.stroke();
    // text
    ctx.fillStyle = "#141414"; ctx.textBaseline = "top";
    for (let k = 0; k < lines.length; k++) {
      const tw = ctx.measureText(lines[k]).width;
      ctx.fillText(lines[k], Math.round(bx + (bw - tw) / 2), by + pad + k * lh);
    }
    ctx.restore();
  }
}

function roundRect(ctx, x, y, w, h, r) {
  r = Math.min(r, w / 2, h / 2);
  ctx.moveTo(x + r, y);
  ctx.arcTo(x + w, y, x + w, y + h, r);
  ctx.arcTo(x + w, y + h, x, y + h, r);
  ctx.arcTo(x, y + h, x, y, r);
  ctx.arcTo(x, y, x + w, y, r);
  ctx.closePath();
}

function withAlpha(hex, a) {
  const n = parseInt(hex.slice(1), 16);
  return `rgba(${(n >> 16) & 255},${(n >> 8) & 255},${n & 255},${a})`;
}
