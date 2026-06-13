// ─────────────────────────────────────────────────────────────────────────
// SimFrancisco · simpler map frontend · orchestration
//
// Flow:  idle → (AI button) → spotlight input → submit
//        → branch + poll the electorate (waiting)
//        → stochastic green/red reveal accumulates on the map
//        → spotlight returns as a result card (distribution) → dismiss → idle
// ─────────────────────────────────────────────────────────────────────────

import { SIM, PREDICT, TIMING } from "./config.js";
import { SF_OUTLINE } from "./sf-outline.js";
import { SFMap } from "./map.js";
import { assignVerdicts } from "./verdict.js";
import * as api from "./api.js";

// ── DOM ──────────────────────────────────────────────────────────────────
const $ = (id) => document.getElementById(id);
const els = {
  canvas: $("map"),
  ai: $("ai-btn"),
  status: $("status"),
  summary: $("summary"),
  summaryText: $("summary-text"),
  summaryLabel: $("summary-label"),
  progress: $("progress"),
  progressFill: $("progress-fill"),
  progressLabel: $("progress-label"),
  spotlight: $("spotlight"),
  inputMode: $("spot-input-mode"),
  resultMode: $("spot-result-mode"),
  input: $("spot-input"),
  toast: $("toast"),
};

const map = new SFMap(els.canvas);
const show = (el) => el.classList.remove("hidden");
const hide = (el) => el.classList.add("hidden");

const state = {
  phase: "booting", // booting | idle | waiting | reveal | results | error
  simId: null,
  mainBranch: null,
  branchId: null,    // current prediction branch
  lastResult: null,
  reqId: 0,          // generation counter — invalidates stale/cancelled async work
  abort: null,       // AbortController for the in-flight prediction
};

// Best-effort delete of the current prediction branch so each prediction owns
// exactly one live branch on the backend (no orphans across "Ask another",
// dismiss, cancel, or poll failure).
function cleanupBranch() {
  if (state.branchId) { api.deleteBranch(state.branchId); state.branchId = null; }
}

// ── boot ───────────────────────────────────────────────────────────────
async function boot() {
  map.setOutline(SF_OUTLINE);
  map.start();
  els.status.textContent = "San Francisco · waking the city…";
  try {
    const sim = await api.createSimulation();
    state.simId = sim.simulation_id;
    state.mainBranch = sim.main_branch;
    const agents = await api.getAllAgents(state.mainBranch);
    if (!agents.length) throw new Error("no agents returned");
    map.setAgents(agents);
    setStatus(agents.length);
    state.phase = "idle";
  } catch (err) {
    console.error(err);
    // Don't leave an empty map — scatter neutral dots inside the outline so the
    // city is still alive, and let the user know predictions are offline.
    map.setAgents(fallbackAgents(SIM.n));
    els.status.textContent = "San Francisco · offline preview (backend unreachable)";
    toast("Couldn't reach the backend — showing an offline preview.");
    state.phase = "error";
  }
}

function setStatus(n) {
  const d = new Date(SIM.start_datetime);
  const date = d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
  els.status.textContent = `San Francisco · ${n.toLocaleString()} residents · ${date}`;
}

// random points inside the outline, for the offline preview only
function fallbackAgents(n) {
  const { bbox } = map.proj;
  const out = [];
  let guard = 0;
  while (out.length < n && guard < n * 40) {
    guard++;
    const lon = bbox.minLon + Math.random() * (bbox.maxLon - bbox.minLon);
    const lat = bbox.minLat + Math.random() * (bbox.maxLat - bbox.minLat);
    if (map.inside.inside(lon, lat)) out.push({ lonlat: [lon, lat] });
  }
  return out;
}

// ── spotlight ────────────────────────────────────────────────────────────
function openInput() {
  if (state.phase === "waiting" || state.phase === "reveal") return;
  cleanupBranch();        // leaving any prior result → release its branch
  map.clearVerdicts();
  hide(els.summary);
  hide(els.resultMode);
  show(els.inputMode);
  show(els.spotlight);
  els.ai.classList.add("active");
  state.phase = "idle";
  requestAnimationFrame(() => { els.input.value = ""; els.input.focus(); });
}

function closeSpotlight() {
  hide(els.spotlight);
  els.ai.classList.remove("active");
}

function dismissResults() {
  closeSpotlight();
  hide(els.summary);
  map.clearVerdicts();
  cleanupBranch();
  state.phase = "idle";
}

// Cancel an in-flight prediction (Esc / AI button during waiting or reveal):
// abort the network call, invalidate its async continuation, clean up, idle.
function cancelPrediction() {
  state.reqId++;                                   // stale-out the running runPrediction
  if (state.abort) { state.abort.abort(); state.abort = null; }
  map.onProgress = null; map.onRevealComplete = null;
  els.ai.classList.remove("busy");
  els.progress.classList.remove("indeterminate");
  hide(els.summary);
  map.clearVerdicts();
  cleanupBranch();
  state.phase = "idle";
}

// ── prediction flow ───────────────────────────────────────────────────────
async function runPrediction(question) {
  question = question.trim();
  if (!question) return;
  if (state.phase === "error" || !state.simId) {
    toast("Predictions need the backend — it's currently unreachable.");
    return;
  }

  cleanupBranch();              // safety: never run two live branches at once
  const myReq = ++state.reqId;  // this prediction's generation
  state.abort = new AbortController();
  const signal = state.abort.signal;
  state.phase = "waiting";
  closeSpotlight();

  // top-right: the "summary" (the question itself — a backend summarize call
  // would slot in here) + an indeterminate progress bar beneath it
  els.summaryLabel.textContent = "PREDICTING";
  els.summaryText.textContent = question;
  els.progressFill.style.width = "18%";
  els.progress.classList.add("indeterminate");
  els.progressLabel.textContent = "tallying the electorate… (esc to cancel)";
  show(els.summary);
  map.setWaiting();
  els.ai.classList.add("busy");

  const framing = /\b(will|won't|by \d{4}|going to)\b/i.test(question) ||
    /^(will|is|are|does|can|could|would)\b/i.test(question) ? "belief" : "vote";

  try {
    // 1) trigger the branching — an isolated clone to poll
    const branch = await api.createBranch(state.simId, {
      ticks: PREDICT.branch_ticks,
      name: "predict",
      signal,
    });
    if (myReq !== state.reqId) return;   // cancelled / superseded
    state.branchId = branch.branch_id;

    // 2) poll the synthetic electorate (the slow LLM pass)
    const result = await api.poll(state.branchId, {
      question,
      framing,
      as_of_date: PREDICT.as_of_date,
      model: PREDICT.model,
    }, signal);
    if (myReq !== state.reqId) return;   // cancelled / superseded

    state.lastResult = { ...result, framing };

    // 3) reveal — stochastic per-dot verdicts whose share == p_yes
    const verdicts = assignVerdicts(map.agents, result.p_yes, question, map.proj.planarSize);
    els.progress.classList.remove("indeterminate");
    els.progressLabel.textContent = `0 / ${map.agents.length.toLocaleString()} responses`;
    map.onProgress = onRevealProgress;
    map.onRevealComplete = () => showResults(state.lastResult);
    state.phase = "reveal";
    map.startReveal(verdicts, TIMING.revealMs);
  } catch (err) {
    if (myReq !== state.reqId) return;   // cancelled — cleanup already handled
    console.error(err);
    toast(`Poll failed: ${err.message}`);
    hide(els.summary);
    els.progress.classList.remove("indeterminate");
    els.ai.classList.remove("busy");
    cleanupBranch();                     // don't orphan the branch we just created
    state.phase = "idle";
  }
}

function onRevealProgress(done, total) {
  const pct = total ? Math.round((done / total) * 100) : 0;
  els.progressFill.style.width = `${Math.max(6, pct)}%`;
  els.progressLabel.textContent = `${done.toLocaleString()} / ${total.toLocaleString()} responses`;
}

// ── result card ────────────────────────────────────────────────────────────
function showResults(result) {
  state.phase = "results";
  els.ai.classList.remove("busy");
  els.summaryLabel.textContent = "RESULT";
  els.progressFill.style.width = "100%";
  els.progressLabel.textContent = `${map.agents.length.toLocaleString()} responses · complete`;

  const pct = Math.round((result.p_yes ?? 0) * 100);
  const noPct = 100 - pct;
  const belief = result.framing === "belief";
  const ciLow = Math.round((result.ci_low ?? result.p_yes) * 100);
  const ciHigh = Math.round((result.ci_high ?? result.p_yes) * 100);
  const n = result.n_agents ?? map.agents.length;
  const rationales = (result.sample_rationales || []).slice(0, 3);

  els.resultMode.innerHTML = `
    <div class="res-q">${escapeHtml(result.question || "")}</div>
    <div class="res-headline">
      <span class="res-pct">${pct}<span class="res-pct-sym">%</span></span>
      <span class="res-verb">${belief ? "likely" : "vote yes"}</span>
    </div>
    <div class="res-bar">
      <div class="res-bar-yes" style="width:${pct}%"></div>
      <div class="res-bar-no" style="width:${noPct}%"></div>
    </div>
    <div class="res-legend">
      <span><i class="dot yes"></i>${belief ? "yes" : "support"} ${pct}%</span>
      <span><i class="dot no"></i>${belief ? "no" : "oppose"} ${noPct}%</span>
    </div>
    <div class="res-meta">${n.toLocaleString()} synthetic residents · 95% CI ${ciLow}–${ciHigh}%</div>
    ${rationales.length ? `<div class="res-why">
      <div class="res-why-label">what people said</div>
      <ul>${rationales.map((r) => `<li>${escapeHtml(r)}</li>`).join("")}</ul>
    </div>` : ""}
    <div class="res-actions">
      <button id="res-again" class="btn btn-primary">Ask another</button>
      <button id="res-dismiss" class="btn">Dismiss</button>
    </div>
  `;
  hide(els.inputMode);
  show(els.resultMode);
  show(els.spotlight);
  const again = $("res-again");
  again.addEventListener("click", openInput);
  $("res-dismiss").addEventListener("click", dismissResults);
  requestAnimationFrame(() => again.focus()); // land keyboard focus in the card
}

// ── misc UI ────────────────────────────────────────────────────────────────
let toastTimer = null;
function toast(msg) {
  els.toast.textContent = msg;
  show(els.toast);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => hide(els.toast), 4200);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// ── events ───────────────────────────────────────────────────────────────
const spotlightHidden = () => els.spotlight.classList.contains("hidden");
const isBusy = () => state.phase === "waiting" || state.phase === "reveal";
const typingTarget = (el) =>
  el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);

els.ai.addEventListener("click", () => {
  if (isBusy()) cancelPrediction();              // busy → clicking the button cancels
  else if (state.phase === "results") dismissResults();
  else if (spotlightHidden()) openInput();
  else closeSpotlight();
});

els.input.addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); runPrediction(els.input.value); }
});

els.spotlight.addEventListener("mousedown", (e) => {
  if (e.target === els.spotlight) {
    if (state.phase === "results") dismissResults();
    else closeSpotlight();
  }
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (isBusy()) cancelPrediction();            // cancel the in-flight poll/reveal
    else if (!spotlightHidden()) {
      state.phase === "results" ? dismissResults() : closeSpotlight();
    }
  } else if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    if (spotlightHidden() && !isBusy()) openInput();
  } else if (e.key === "/" && spotlightHidden() && !isBusy() && !typingTarget(document.activeElement)) {
    e.preventDefault();
    openInput();
  }
});

boot();
