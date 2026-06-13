// ─────────────────────────────────────────────────────────────────────────
// Per-agent green/red assignment for the poll map.
//
// IMPORTANT: the backend's /poll returns an *aggregate* (p_yes + demographic
// breakdowns), not a verdict per agent. Per the simpler-frontend spec the
// per-dot result is a stochastic visualization. We assign each dot yes/no so
// that (a) the on-screen yes-share equals p_yes exactly, and (b) the colors
// form organic regions rather than salt-and-pepper noise — so it reads like a
// real poll map. A smooth value-noise field over the map gives the clustering;
// thresholding it at the p_yes quantile gives the exact marginal.
//
// Seeded by the question text → identical map for an identical question, which
// mirrors the backend's deterministic poll cache.
// ─────────────────────────────────────────────────────────────────────────

function hashString(s) {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function mulberry32(a) {
  return function () {
    a |= 0; a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// Smooth-ish scalar field from a handful of randomly-oriented sine waves.
function makeField(rng, w, h) {
  const waves = [];
  for (let i = 0; i < 5; i++) {
    const freq = (0.6 + rng() * 2.4) / Math.max(w, h);
    const ang = rng() * Math.PI * 2;
    waves.push({
      kx: Math.cos(ang) * freq * Math.PI * 2,
      ky: Math.sin(ang) * freq * Math.PI * 2,
      ph: rng() * Math.PI * 2,
      amp: 0.5 + rng() * 0.5,
    });
  }
  return (x, y) => {
    let v = 0;
    for (const wv of waves) v += wv.amp * Math.sin(x * wv.kx + y * wv.ky + wv.ph);
    return v;
  };
}

// agents: [{ hx, hy }] in planar units. Returns Array<'yes'|'no'> aligned to agents.
export function assignVerdicts(agents, pYes, question, planarSize) {
  const rng = mulberry32(hashString(question || "predict") ^ 0x9e3779b9);
  const field = makeField(rng, planarSize.w, planarSize.h);

  // field value + a little per-agent jitter so region edges aren't razor sharp
  const scored = agents.map((a, i) => ({
    i,
    v: field(a.hx, a.hy) + (rng() - 0.5) * 0.7,
  }));
  scored.sort((p, q) => p.v - q.v);

  const yesCount = Math.round(clamp01(pYes) * agents.length);
  const verdict = new Array(agents.length).fill("no");
  for (let k = 0; k < yesCount; k++) verdict[scored[k].i] = "yes";
  return verdict;
}

function clamp01(x) { return x < 0 ? 0 : x > 1 ? 1 : x; }
