// ─────────────────────────────────────────────────────────────────────────
// sim francisco · pixel-map frontend · orchestration
//
// Flow:  idle → click "ask" (bottom-center) → multiline composer
//        → submit → branch + poll the electorate (composer shows "predicting…")
//        → stochastic green/red verdicts pop over the sprite crowd
//        → result card expands above the composer → dismiss → idle
// Map:   click anywhere → camera zooms into that spot (sprites walk the roads);
//        the "whole city" button returns to the overview.
// ─────────────────────────────────────────────────────────────────────────

import { SIM, PREDICT, TIMING } from "./config.js";
import { SFMap } from "./map.js";
import { assignVerdicts } from "./verdict.js";
import * as api from "./api.js";

const $ = (id) => document.getElementById(id);
const els = {
  canvas: $("map"),
  status: $("status"),
  returnBtn: $("return"),
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
  infoBtn: $("info-btn"),
  about: $("about"),
  aboutScrim: $("about-scrim"),
  aboutClose: $("about-close"),
};

const map = new SFMap(els.canvas);
const show = (el) => el.classList.remove("hidden");
const hide = (el) => el.classList.add("hidden");

const state = {
  phase: "booting",
  simId: null, mainBranch: null, branchId: null,
  lastResult: null, reqId: 0, abort: null,
  residents: SIM.n,
};

const isBusy = () => state.phase === "waiting" || state.phase === "reveal";
const inputOpen = () => els.ask.dataset.state === "input";

function setAsk(s) {
  els.ask.dataset.state = s;
  els.askLabel.textContent = s === "busy" ? "predicting…" : "ask";
}

function cleanupBranch() {
  if (state.branchId) { api.deleteBranch(state.branchId); state.branchId = null; }
}

// ── boot ───────────────────────────────────────────────────────────────
async function boot() {
  map.onZoomChange = (zoomedIn) => { zoomedIn ? show(els.returnBtn) : hide(els.returnBtn); };
  map.start();
  els.status.textContent = "waking the city…";
  try {
    const sim = await api.createSimulation();
    state.simId = sim.simulation_id;
    state.mainBranch = sim.main_branch;
    const agents = await api.getAllAgents(state.mainBranch);
    if (!agents.length) throw new Error("no agents returned");
    map.setAgents(agents);
    state.residents = agents.length;
    setIdleStatus();
    state.phase = "idle";
  } catch (err) {
    console.error(err);
    map.setAgents(fallbackAgents(SIM.n));        // never leave an empty city
    els.status.textContent = "offline preview · backend unreachable";
    toast("Couldn't reach the backend — showing an offline preview.");
    state.phase = "error";
  }
}

function setIdleStatus() {
  const d = new Date(SIM.start_datetime);
  const date = d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
  els.status.textContent = `san francisco · ${state.residents.toLocaleString()} residents · ${date}`;
  show(els.status);
}

// random points inside the map bbox, for the offline preview only
function fallbackAgents(n) {
  const { bbox } = map.proj;
  const out = [];
  for (let i = 0; i < n; i++) {
    const lon = bbox.minLon + Math.random() * (bbox.maxLon - bbox.minLon);
    const lat = bbox.minLat + Math.random() * (bbox.maxLat - bbox.minLat);
    out.push({ lonlat: [lon, lat] });
  }
  return out;
}

// ── ask composer (multiline) ───────────────────────────────────────────
function autoGrow() {
  const ta = els.askInput;
  ta.style.height = "auto";
  ta.style.height = Math.min(ta.scrollHeight, Math.round(window.innerHeight * 0.4)) + "px";
}

function openInput() {
  if (isBusy()) return;
  if (state.phase === "error" || !state.simId) { toast("Predictions need the backend — it's currently unreachable."); return; }
  cleanupBranch();
  map.clearVerdicts();
  hide(els.summary);
  hide(els.resultCard);
  setAsk("input");
  state.phase = "idle";
  setIdleStatus();
  requestAnimationFrame(() => { els.askInput.value = ""; autoGrow(); els.askInput.focus(); });
}

function closeInput() {
  setAsk("idle");
  els.askInput.value = "";
  els.askInput.style.height = "auto";
  els.askInput.blur();
}

function dismissResults() {
  hide(els.resultCard);
  hide(els.summary);
  map.clearVerdicts();
  cleanupBranch();
  setAsk("idle");
  setIdleStatus();
  state.phase = "idle";
}

function cancelPrediction() {
  state.reqId++;
  if (state.abort) { state.abort.abort(); state.abort = null; }
  map.onProgress = null; map.onRevealComplete = null;
  els.progress.classList.remove("indeterminate");
  hide(els.summary);
  hide(els.resultCard);
  map.clearVerdicts();
  cleanupBranch();
  setAsk("idle");
  setIdleStatus();
  state.phase = "idle";
}

// ── prediction flow ─────────────────────────────────────────────────────
async function runPrediction(question) {
  question = (question || "").trim();
  if (!question) return;
  if (state.phase === "error" || !state.simId) { toast("Predictions need the backend — it's currently unreachable."); return; }

  cleanupBranch();
  const myReq = ++state.reqId;
  state.abort = new AbortController();
  const signal = state.abort.signal;
  state.phase = "waiting";
  setAsk("busy");
  els.askInput.blur();

  els.summaryLabel.textContent = "PREDICTING";
  els.summaryText.textContent = question;
  els.progressFill.style.width = "18%";
  els.progress.classList.add("indeterminate");
  els.progressLabel.textContent = "tallying the electorate… (esc to cancel)";
  show(els.summary);
  map.setWaiting();

  const framing = /\b(will|won't|by \d{4}|going to)\b/i.test(question) ||
    /^(will|is|are|does|do|can|could|would|should)\b/i.test(question) ? "belief" : "vote";

  try {
    const branch = await api.createBranch(state.simId, { ticks: PREDICT.branch_ticks, name: "predict", signal });
    if (myReq !== state.reqId) { api.deleteBranch(branch.branch_id); return; }
    state.branchId = branch.branch_id;

    const result = await api.poll(state.branchId, {
      question, framing, as_of_date: PREDICT.as_of_date, model: PREDICT.model,
    }, signal);
    if (myReq !== state.reqId) return;

    state.lastResult = { ...result, framing };

    const verdicts = assignVerdicts(map.agents, result.p_yes, question, map.proj.planarSize);
    els.progress.classList.remove("indeterminate");
    els.progressLabel.textContent = `0 / ${map.agents.length.toLocaleString()} responses`;
    map.onProgress = onRevealProgress;
    map.onRevealComplete = () => { if (myReq === state.reqId) showResults(state.lastResult); };
    state.phase = "reveal";
    map.startReveal(verdicts, TIMING.revealMs);
  } catch (err) {
    if (myReq !== state.reqId) return;
    console.error(err);
    toast(`Poll failed: ${err.message}`);
    hide(els.summary);
    els.progress.classList.remove("indeterminate");
    cleanupBranch();
    setAsk("idle");
    setIdleStatus();
    state.phase = "idle";
  }
}

function onRevealProgress(done, total) {
  const pct = total ? Math.round((done / total) * 100) : 0;
  els.progressFill.style.width = `${Math.max(6, pct)}%`;
  els.progressLabel.textContent = `${done.toLocaleString()} / ${total.toLocaleString()} responses`;
}

// ── result card ──────────────────────────────────────────────────────────
function showResults(result) {
  state.phase = "results";
  setAsk("idle");
  els.progressFill.style.width = "100%";
  hide(els.summary);
  clearTimeout(toastTimer); hide(els.toast);

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
  requestAnimationFrame(() => again.focus());
}

// ── misc ─────────────────────────────────────────────────────────────────
let toastTimer = null;
function toast(msg) {
  els.toast.textContent = msg;
  show(els.toast);
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => hide(els.toast), 4200);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

const typingTarget = (el) => el && (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable);

// ── events ───────────────────────────────────────────────────────────────
els.ask.addEventListener("click", () => {
  if (isBusy()) { cancelPrediction(); return; }
  if (inputOpen()) { els.askInput.focus(); return; }
  openInput();
});

// multiline composer: Enter submits, Shift+Enter inserts a newline
els.askInput.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); runPrediction(els.askInput.value); }
});
els.askInput.addEventListener("input", autoGrow);

els.returnBtn.addEventListener("click", () => { map.returnToOverview(); });

// about card (the ? button)
const aboutOpen = () => !els.about.classList.contains("hidden");
function openAbout() { show(els.about); show(els.aboutScrim); }
function closeAbout() { hide(els.about); hide(els.aboutScrim); }
els.infoBtn.addEventListener("click", openAbout);
els.aboutClose.addEventListener("click", closeAbout);
els.aboutScrim.addEventListener("click", closeAbout);

// click outside the dock collapses an open composer
document.addEventListener("mousedown", (e) => {
  if (inputOpen() && !els.dock.contains(e.target)) closeInput();
});

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (aboutOpen()) closeAbout();
    else if (isBusy()) cancelPrediction();
    else if (state.phase === "results") dismissResults();
    else if (inputOpen()) closeInput();
    else if (map.zoomedIn) map.returnToOverview();
  } else if (e.key === "k" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    if (!isBusy() && !inputOpen()) openInput();
  } else if (e.key === "/" && !isBusy() && !inputOpen() && !typingTarget(document.activeElement)) {
    e.preventDefault();
    openInput();
  }
});

boot();
