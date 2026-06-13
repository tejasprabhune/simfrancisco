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
  start_datetime: "2024-11-01T08:00:00Z",
  tick_seconds: 30,
};

// Per-prediction branch + poll defaults.
export const PREDICT = {
  branch_ticks: 2,        // tiny "what-if" run so the branch reflects the event
  as_of_date: "2024-11-04",
  model: "gpt-4o",
};

// Animation timing (ms).
export const TIMING = {
  revealMs: 4800,         // how long the green/red dots take to fully accumulate
  driftMs: 9000,          // ambient drift cycle
  fadeBackMs: 900,        // verdict -> neutral when results are dismissed
};

// Fog & Bay palette. Mirrored in styles.css as CSS custom properties — keep
// the two in sync if you retheme.
export const COLORS = {
  water:   "#EEF2F6",     // background / bay + ocean
  land:    "#FCFDFE",     // the SF landmass fill
  ink:     "#2B3440",     // coastline stroke + primary text
  inkSoft: "#8A95A3",     // neutral dots + secondary text
  accent:  "#5B6CFF",     // "magic AI" indigo
  yes:     "#2E9E5B",     // strong yes
  no:      "#D1495B",     // strong no
};
