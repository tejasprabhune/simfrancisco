// ─────────────────────────────────────────────────────────────────────────
// sim francisco · simpler map frontend · orchestration
//
// Flow:  idle → click "ask" (bottom-center) → bar expands to "predict anything"
//        → submit → branch + poll the electorate (bar shows "predicting…")
//        → stochastic green/red reveal accumulates on the map (progress top-right)
//        → a result card expands above the bar (distribution) → dismiss → idle
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
  status: $("status"),
  summary: $("summary"),
  summaryLabel: $("summary-label"),
  summaryText: $("summary-text"),
  progress: $("progress"),
  progressFill: $("progress-fill"),
  progressLabel: $("progress-label"),
  dock: $("dock"),
  ask: $("ask"),
  askInput: $("ask-input"),
  askLabel: $("ask-label"),
  resultCard: $("result-card"),
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

const isBusy = () => state.phase === "waiting" || state.phase === "reveal";
const inputOpen = () => els.ask.dataset.state === "input";

// drive the bottom bar's three visual states (idle pill · input · busy)
function setAsk(s) {
  els.ask.dataset.state = s;
  els.askLabel.textContent = s === "busy" ? "predicting…" : "ask";
  document.body.classList.toggle("dock-active", s !== "idle"); // fades the ambient status out of the way
}

// Best-effort delete of the current prediction branch so each prediction owns
// exactly one live branch on the backend (no orphans across ask-again, dismiss,
// cancel, or poll failure).
function cleanupBranch() {
  if (state.branchId) { api.deleteBranch(state.branchId); state.branchId = null; }
}

// ── boot ───────────────────────────────────────────────────────────────
async function boot() {
  map.setOutline(SF_OUTLINE);
  map.start();
  els.status.textContent = "waking the city…";
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
    els.status.textContent = "offline preview · backend unreachable";
    toast("Couldn't reach the backend — showing an offline preview.");
    state.phase = "error";
  }
}

function setStatus(n) {
  const d = new Date(SIM.start_datetime);
  const date = d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
  els.status.textContent = `san francisco · ${n.toLocaleString()} residents · ${date}`;
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

// ── ask bar ────────────────────────────────────────────────────────────
function openInput() {
  if (isBusy()) return;
  if (state.phase === "error" || !state.simId) {   // backend offline → don't open a dead input
    toast("Predictions need the backend — it's currently unreachable.");
    return;
  }
  cleanupBranch();        // leaving any prior result → release its branch
  map.clearVerdicts();
  hide(els.summary);
  hide(els.resultCard);
  setAsk("input");
  state.phase = "idle";
  requestAnimationFrame(() => { els.askInput.value = ""; els.askInput.focus(); });
}

function closeInput() {
  setAsk("idle");
  els.askInput.value = "";
  els.askInput.blur();
}

function dismissResults() {
  hide(els.resultCard);
  hide(els.summary);
  map.clearVerdicts();
  cleanupBranch();
  setAsk("idle");
  state.phase = "idle";
}

// Cancel an in-flight prediction (Esc / clicking the busy bar): abort the
// network call, invalidate its async continuation, clean up, return to idle.
function cancelPrediction() {
  state.reqId++;                                   // stale-out the running runPrediction
  if (state.abort) { state.abort.abort(); state.abort = null; }
  map.onProgress = null; map.onRevealComplete = null;
  els.progress.classList.remove("indeterminate");
  hide(els.summary);
  hide(els.resultCard);
  map.clearVerdicts();
  cleanupBranch();
  setAsk("idle");
  state.phase = "idle";
}

// ── prediction flow ───────────────────────────────────────────────────────
async function runPrediction(question) {
  question = (question || "").trim();
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
  setAsk("busy");
  els.askInput.blur();

  // top-right: the query "summary" (the question itself — a backend summarize
  // call would slot in here) + a progress bar beneath it
  els.summaryLabel.textContent = "PREDICTING";
  els.summaryText.textContent = question;
  els.progressFill.style.width = "18%";
  els.progress.classList.add("indeterminate");
  els.progressLabel.textContent = "tallying the electorate… (esc to cancel)";
  show(els.summary);
  map.setWaiting();

  const framing = /\b(will|won't|by \d{4}|going to)\b/i.test(question) ||
    /^(will|is|are|does|can|could|would)\b/i.test(question) ? "belief" : "vote";

  try {
    // 1) trigger the branching — an isolated clone to poll
    const branch = await api.createBranch(state.simId, {
      ticks: PREDICT.branch_ticks,
      name: "predict",
      signal,
    });
    if (myReq !== state.reqId) {          // cancelled / superseded mid-create
      api.deleteBranch(branch.branch_id); // release the branch we just made (id not yet stored)
      return;
    }
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
    map.onRevealComplete = () => { if (myReq === state.reqId) showResults(state.lastResult); };
    state.phase = "reveal";
    map.startReveal(verdicts, TIMING.revealMs);
  } catch (err) {
    if (myReq !== state.reqId) return;   // cancelled — cleanup already handled
    console.error(err);
    toast(`Poll failed: ${err.message}`);
    hide(els.summary);
    els.progress.classList.remove("indeterminate");
    cleanupBranch();                     // don't orphan the branch we just created
    setAsk("idle");
    state.phase = "idle";
  }
}

function onRevealProgress(done, total) {
  const pct = total ? Math.round((done / total) * 100) : 0;
  els.progressFill.style.width = `${Math.max(6, pct)}%`;
  els.progressLabel.textContent = `${done.toLocaleString()} / ${total.toLocaleString()} responses`;
}

// ── result card (expands above the ask bar) ─────────────────────────────────
function showResults(result) {
  state.phase = "results";
  setAsk("idle");
  els.progressFill.style.width = "100%";
  hide(els.summary);                     // the card below now carries everything
  clearTimeout(toastTimer); hide(els.toast); // a stale error toast must not overlap the card

  const pct = Math.round((result.p_yes ?? 0) * 100);
  const noPct = 100 - pct;
  const belief = result.framing === "belief";
  const ciLow = Math.round((result.ci_low ?? result.p_yes) * 100);
  const ciHigh = Math.round((result.ci_high ?? result.p_yes) * 100);
  const n = result.n_agents ?? map.agents.length;
  const rationales = (result.sample_rationales || []).slice(0, 3);

  els.resultCard.innerHTML = `
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
  show(els.resultCard);
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

const typingTarget = (el) =>
  el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);

// ── events ───────────────────────────────────────────────────────────────
els.ask.addEventListener("click", () => {
  if (isBusy()) { cancelPrediction(); return; }   // clicking the busy bar cancels
  if (inputOpen()) { els.askInput.focus(); return; }
  openInput();                                     // idle or results → open input
});

els.askInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); runPrediction(els.askInput.value); }
});

// click outside the dock collapses an open input
document.addEventListener("mousedown", (e) => {
  if (inputOpen() && !els.dock.contains(e.target)) closeInput();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (isBusy()) cancelPrediction();              // cancel the in-flight poll/reveal
    else if (state.phase === "results") dismissResults();
    else if (inputOpen()) closeInput();
  } else if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    if (!isBusy() && !inputOpen()) openInput();
  } else if (e.key === "/" && !isBusy() && !inputOpen() && !typingTarget(document.activeElement)) {
    e.preventDefault();
    openInput();
  }
});

boot();
