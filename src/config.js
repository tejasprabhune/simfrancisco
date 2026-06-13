// ─────────────────────────────────────────────────────────────────────────
// SimFrancisco · simpler map frontend · configuration
// ─────────────────────────────────────────────────────────────────────────

// Backend (see INTEGRATION.md). CORS is wide-open, so browser fetch works.
export const BASE = "https://sf-digital-twin-tp.fly.dev";

// Synthetic population to spin up on load. n>=~1200 keeps demographics in
// tolerance; that's also a comfortable number of dots to render.
export const SIM = {
  n: 1200,
  seed: 42,
  start_datetime: "2026-06-13T08:00:00Z",
  tick_seconds: 30,
};

// Per-prediction branch + poll defaults.
export const PREDICT = {
  branch_ticks: 2,        // tiny "what-if" run so the branch reflects the event
  as_of_date: "2024-11-04",
  model: "gpt-4o",
};

// Animation timing (ms). UI animations are deliberately snappy; the poll-dot
// accumulation stays a few seconds because the "results slowly popping up" is
// the core data-viz moment.
export const TIMING = {
  revealMs: 3000,         // how long the green/red dots take to fully accumulate
  driftMs: 9000,          // ambient drift cycle
  fadeBackMs: 420,        // verdict -> neutral when results are dismissed
};

// Palette sampled from the provided pixel-art palette image (Downloads/Palette.png)
// and assigned to UI/map roles. Mirrored in styles.css custom properties — keep
// the two in sync if you retheme.
export const COLORS = {
  water:    "#C9DCE8",    // bay + ocean (page background) — pale palette blue
  waterTop: "#DDEBF1",    // subtle gradient toward the top
  land:     "#E7DABF",    // the SF landmass fill — warm palette sand
  ink:      "#2F3242",    // coastline + shadow + primary text — deep palette navy
  inkSoft:  "#8E93A8",    // neutral / undecided dots — palette blue-gray
  accent:   "#7758AB",    // "magic AI" — palette violet
  yes:      "#2E9B4E",    // strong yes — palette green
  no:       "#C0352F",    // strong no — palette brick red
};
