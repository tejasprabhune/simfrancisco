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

import { SIM, PREDICT, TIMING, MAP } from "./config.js";
import { SFMap } from "./map.js";
import { assignVerdicts } from "./verdict.js";
import * as api from "./api.js";

const $ = (id) => document.getElementById(id);
const els = {
  canvas: $("map"),
  titleSelect: $("title-select"),
  titleBtn: $("title-btn"),
  titleCurrent: $("title-current"),
  titleMenu: $("title-menu"),
  status: $("status"),
  newsBubble: $("news-bubble"),
  boot: $("boot"),
  bootFill: $("boot-fill"),
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
  charCard: $("char-card"),
};

const map = new SFMap(els.canvas);
const show = (el) => el.classList.remove("hidden");
const hide = (el) => el.classList.add("hidden");

const state = {
  phase: "booting",
  simId: null, mainBranch: null, branchId: null,
  lastResult: null, reqId: 0, abort: null,
  residents: SIM.n,
  cities: [],            // [{slug, display, bbox, ...}] from GET /cities
  city: null,            // the active city object (falls back to a synthetic "sf")
  switching: false,      // true while a city swap is re-creating the simulation
  news: [],              // the active city's recent articles (expandable bubble)
  newsExpanded: false,   // whether the news bubble is showing all of them
};

// fallback city when /cities is unavailable — keeps the single-city SF behavior.
const SF_FALLBACK = { slug: "sf", display: "sim francisco", bbox: { ...MAP.bbox }, default: true };
const citySlug = () => state.city?.slug || "sf";

// fetch LLM chatter for the residents now on screen (sparse, batched, best-effort)
async function requestChatter(ids) {
  if (!state.mainBranch || !ids?.length) return;
  const branch = state.mainBranch;
  try {
    const data = await api.getChatter(branch, ids);
    if (branch !== state.mainBranch) return;            // city swapped mid-flight — drop it
    const ch = data?.chatter || {};
    for (const [id, text] of Object.entries(ch)) map.setThought(Number(id), text);
  } catch { /* best-effort: residents keep their neutral fallback thought */ }
}
map.onNeedChatter = requestChatter;

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
// 0 → 1 progress on the thin boot bar. createSim is one slow step (~0.2); the
// agent fetch is paged, so it fills the rest (0.2 → 0.97) as residents arrive.
function setBoot(p) { els.bootFill.style.width = `${Math.round(Math.max(0, Math.min(1, p)) * 100)}%`; }

async function boot() {
  map.onZoomChange = (zoomedIn) => { zoomedIn ? show(els.returnBtn) : hide(els.returnBtn); };
  map.start();
  els.status.textContent = "waking the city…";

  // Load the city catalog first (best-effort). If it fails we keep the existing
  // single-city SF behavior — the switcher just stays hidden.
  let initial = SF_FALLBACK;
  try {
    const data = await api.getCities();
    const cities = (data?.cities || []).filter((c) => c && c.slug);
    if (cities.length) {
      state.cities = cities;
      initial = cities.find((c) => c.default) || cities[0];
      buildTitleSelect();
    }
  } catch (err) {
    console.warn("city catalog unavailable, falling back to SF:", err);
  }

  await loadCity(initial);
}

// Create (or re-create) the simulation for a city, point the map base/bbox at it,
// load that city's agents and reset the overview. Shared by boot + the switcher.
async function loadCity(city) {
  state.city = city;
  // point the renderer at this city's tiles + bbox before agents are placed
  MAP.base = `assets/${city.slug}_tiles.png`;
  if (city.bbox) MAP.bbox = { ...city.bbox };
  map.setBase(MAP.base);
  syncActiveTitle();

  els.status.textContent = `waking ${city.display}…`;
  hide(els.newsBubble);            // clear the previous city's news while loading
  show(els.boot); setBoot(0.06);
  try {
    const sim = await api.createSimulation({ city: city.slug });
    state.simId = sim.simulation_id;
    state.mainBranch = sim.main_branch;
    setBoot(0.2);
    const agents = await api.getAllAgents(state.mainBranch, (loaded, total) => {
      setBoot(0.2 + 0.77 * (total ? loaded / total : 0));
    });
    if (!agents.length) throw new Error("no agents returned");
    map.setAgents(agents);
    state.residents = agents.length;
    map.setSim(city.slug, state.mainBranch);     // scope ambient chatter to this city + branch
    setBoot(1);
    setIdleStatus();
    // let the bar finish, fade it out, then surface the news in its place (no overlap)
    setTimeout(() => { hide(els.boot); loadNews(city.slug); }, 450);
    state.phase = "idle";
  } catch (err) {
    console.error(err);
    hide(els.boot);
    hide(els.newsBubble);
    state.simId = null; state.mainBranch = null;
    map.setAgents(fallbackAgents(SIM.n));        // never leave an empty city
    els.status.textContent = "offline preview · backend unreachable";
    toast("Couldn't reach the backend — showing an offline preview.");
    state.phase = "error";
  }
}

function setIdleStatus() {
  const n = state.residents.toLocaleString();
  const display = (state.city?.display || "san francisco").toLowerCase();
  const kd = state.city?.knowledge_date;
  // the clock = the date up to which the residents know the news (their knowledge cutoff)
  const clock = kd
    ? `<span class="status-clock">residents know the news up to ${escapeHtml(fmtDate(kd))}</span>`
    : "";
  if (window.innerWidth < 560) {
    els.status.innerHTML = `${n} residents`;              // compact on phones
  } else {
    els.status.innerHTML = `${escapeHtml(display)} · ${n} residents${clock}`;
  }
  show(els.status);
}

function fmtDate(iso) {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  return d.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
}

// fetch the city's recent news into the bubble (best-effort); click to expand all
async function loadNews(slug) {
  state.newsExpanded = false;
  try {
    const data = await api.getNews(slug);
    state.news = data.articles || [];
    if (!state.news.length) { hide(els.newsBubble); return; }
    renderNews();
    show(els.newsBubble);
  } catch {
    state.news = [];
    hide(els.newsBubble);
  }
}

// render the news bubble in its current (collapsed / expanded) state
function renderNews() {
  const arts = state.news;
  if (!arts.length) { hide(els.newsBubble); return; }
  els.newsBubble.dataset.expanded = state.newsExpanded ? "true" : "false";
  const caret = arts.length > 1
    ? `<svg class="news-toggle" viewBox="0 0 24 24" width="13" height="13" aria-hidden="true"><path fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" d="M6 9l6 6 6-6"/></svg>`
    : "";
  const head = `<span class="news-head"><span>informing the residents</span>${caret}</span>`;
  const body = state.newsExpanded
    ? arts.map((a) =>
        `<div class="news-art">` +
        (a.date ? `<span class="news-art-date">${escapeHtml(fmtDate(a.date))}</span>` : "") +
        `<span class="news-art-head">${escapeHtml(a.headline)}</span>` +
        (a.summary ? `<span class="news-art-sum">${escapeHtml(a.summary)}</span>` : "") +
        `</div>`
      ).join("")
    : arts.slice(0, 3).map((a) => `<span class="news-item">${escapeHtml(a.headline)}</span>`).join("");
  els.newsBubble.innerHTML = head + body;
}

// click the bubble to expand it to all the news updating the residents (and back)
els.newsBubble.addEventListener("click", () => {
  if (!state.news.length) return;
  state.newsExpanded = !state.newsExpanded;
  renderNews();
});

// ── title-select: the title itself is the city switcher ────────────────────
function buildTitleSelect() {
  els.titleMenu.innerHTML = state.cities.map((c) =>
    `<button class="title-option" type="button" role="option" data-slug="${escapeHtml(c.slug)}"
       aria-selected="false">${escapeHtml(c.display)}</button>`
  ).join("");
  els.titleMenu.querySelectorAll(".title-option").forEach((btn) => {
    btn.addEventListener("click", () => { closeTitleMenu(); onSelectCity(btn.dataset.slug); });
  });
  syncActiveTitle();
}

function titleMenuOpen() { return els.titleBtn.getAttribute("aria-expanded") === "true"; }
function openTitleMenu() {
  if (state.cities.length <= 1 || state.switching) return;
  show(els.titleMenu);
  els.titleBtn.setAttribute("aria-expanded", "true");
}
function closeTitleMenu() {
  hide(els.titleMenu);
  els.titleBtn.setAttribute("aria-expanded", "false");
}
function toggleTitleMenu() { titleMenuOpen() ? closeTitleMenu() : openTitleMenu(); }

// reflect the active city in the title button + the menu; lock while swapping
function syncActiveTitle() {
  const city = state.cities.find((c) => c.slug === citySlug());
  els.titleCurrent.textContent = city?.display || state.city?.display || "sim francisco";
  els.titleMenu.querySelectorAll(".title-option").forEach((btn) => {
    btn.setAttribute("aria-selected", btn.dataset.slug === citySlug() ? "true" : "false");
    btn.disabled = state.switching;
  });
  els.titleBtn.disabled = state.switching || state.cities.length <= 1;
}

// title-button toggles the dropdown; click-away / Escape close it
els.titleBtn.addEventListener("click", (e) => {
  e.stopPropagation();
  if (!els.titleBtn.disabled) toggleTitleMenu();
});
document.addEventListener("click", (e) => {
  if (titleMenuOpen() && !els.titleSelect.contains(e.target)) closeTitleMenu();
});
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && titleMenuOpen()) closeTitleMenu();
});

async function onSelectCity(slug) {
  if (state.switching || slug === citySlug()) return;
  const city = state.cities.find((c) => c.slug === slug);
  if (!city) return;

  // tear down any in-flight prediction / lingering UI from the previous city
  state.reqId++;
  if (state.abort) { state.abort.abort(); state.abort = null; }
  map.onProgress = null; map.onRevealComplete = null;
  els.progress.classList.remove("indeterminate");
  cleanupBranch();
  closeCharCard();
  hide(els.summary); hide(els.resultCard);
  if (inputOpen()) closeInput();

  state.switching = true;
  state.phase = "booting";
  syncActiveTitle();
  try {
    await loadCity(city);
  } finally {
    state.switching = false;
    syncActiveTitle();
  }
}
// keep the status text right-sized across orientation changes
window.addEventListener("resize", () => {
  if (state.phase === "idle" || state.phase === "results") setIdleStatus();
});

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
// The composer opens at exactly one line and expands only HORIZONTALLY; it grows
// vertically solely when the typed text wraps past a single line.
const LINE_H = 24;
function autoGrow() {
  const ta = els.askInput;
  ta.style.height = LINE_H + "px";          // reset to one line, then measure
  const sh = ta.scrollHeight;
  if (sh > LINE_H + 1) {
    const cap = Math.round(window.innerHeight * 0.4);
    const h = Math.min(sh, cap);
    ta.style.height = h + "px";
    ta.style.overflowY = h >= cap ? "auto" : "hidden";
  } else {
    ta.style.overflowY = "hidden";
  }
}

function openInput() {
  if (isBusy()) return;
  if (state.phase === "error" || !state.simId) { toast("Predictions need the backend — it's currently unreachable."); return; }
  cleanupBranch();
  map.clearVerdicts();
  closeCharCard();
  hide(els.summary);
  hide(els.resultCard);
  setAsk("input");
  state.phase = "idle";
  setIdleStatus();
  requestAnimationFrame(() => { els.askInput.value = ""; els.askInput.style.height = LINE_H + "px"; els.askInput.focus(); });
}

function closeInput() {
  setAsk("idle");
  els.askInput.value = "";
  els.askInput.style.height = LINE_H + "px";
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

// heuristic framing, used only as a fallback when /parse is unavailable
function guessFraming(question) {
  return /\b(will|won't|by \d{4}|going to)\b/i.test(question) ||
    /^(will|is|are|does|do|can|could|would|should)\b/i.test(question) ? "belief" : "vote";
}

// ── prediction flow ─────────────────────────────────────────────────────
// 1) classify the question for the current city (POST /cities/<slug>/parse)
// 2) if unsupported → a gentle "try rephrasing" card (no poll)
// 3) if supported → poll the branch with the parsed {framing, question, description, options}
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

  els.summaryLabel.textContent = "READING";
  els.summaryText.textContent = question;
  els.progressFill.style.width = "12%";
  els.progress.classList.add("indeterminate");
  els.progressLabel.textContent = "understanding your question… (esc to cancel)";
  hide(els.resultCard);
  show(els.summary);
  map.setWaiting();

  try {
    // classify first; fall back to a heuristic binary framing if /parse is missing
    let parsed = null;
    try {
      parsed = await api.parseQuestion(citySlug(), question, signal);
    } catch (perr) {
      console.warn("parse unavailable, falling back to binary framing:", perr);
    }
    if (myReq !== state.reqId) return;

    if (parsed && parsed.supported === false) {
      showRephrase(parsed, question);
      return;
    }

    const framing = parsed?.framing || guessFraming(question);
    const description = parsed?.description || "";
    const options = parsed?.options && parsed.options.length ? parsed.options : undefined;
    const pollQuestion = parsed?.question || question;

    els.summaryLabel.textContent = "PREDICTING";
    els.progressFill.style.width = "18%";
    els.progressLabel.textContent = "tallying the electorate… (esc to cancel)";

    const branch = await api.createBranch(state.simId, { ticks: PREDICT.branch_ticks, name: "predict", signal });
    if (myReq !== state.reqId) { api.deleteBranch(branch.branch_id); return; }
    state.branchId = branch.branch_id;

    const result = await api.poll(state.branchId, {
      question: pollQuestion, description, framing, ...(options ? { options } : {}),
      as_of_date: PREDICT.as_of_date, model: PREDICT.model,
    }, signal);
    if (myReq !== state.reqId) return;

    state.lastResult = { ...result, framing, question: pollQuestion };

    // p_yes drives the on-map green/red reveal for both paths; for options it is the
    // winning option's share, so the crowd still visualizes the result's strength.
    const verdicts = assignVerdicts(map.agents, result.p_yes, pollQuestion, map.proj.planarSize);
    map.setRationales(result.sample_rationales);   // real per-agent reasoning → thought bubbles
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

  if (result.framing === "options" && Array.isArray(result.p_distribution) && result.p_distribution.length) {
    showOptionResults(result);
    return;
  }

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
  wireResultActions();
}

// the "Ask another / Dismiss" footer is identical across every result card
function wireResultActions(focusEl) {
  const again = $("res-again");
  again.addEventListener("click", openInput);
  $("res-dismiss").addEventListener("click", dismissResults);
  requestAnimationFrame(() => (focusEl || again).focus());
}

const RESULT_ACTIONS = `
  <div class="res-actions">
    <button id="res-again" class="btn btn-primary">Ask another</button>
    <button id="res-dismiss" class="btn">Dismiss</button>
  </div>`;

// multi-option result: a horizontal bar per option (sorted desc), winner emphasized
function showOptionResults(result) {
  const dist = (result.p_distribution || [])
    .filter((d) => Array.isArray(d) && d.length >= 2)
    .map(([label, p]) => ({ label: String(label), p: Number(p) || 0 }))
    .sort((a, b) => b.p - a.p);
  const n = result.n_agents ?? map.agents.length;
  const rationales = (result.sample_rationales || []).slice(0, 3);

  const rows = dist.map((d, i) => {
    const pct = Math.round(d.p * 100);
    return `
      <div class="res-opt${i === 0 ? " win" : ""}">
        <div class="res-opt-head">
          <span class="res-opt-label">${escapeHtml(d.label)}</span>
          <span class="res-opt-pct">${pct}%</span>
        </div>
        <div class="res-opt-track"><div class="res-opt-fill" style="width:${pct}%"></div></div>
      </div>`;
  }).join("");

  els.resultCard.innerHTML = `
    <div class="res-q">${escapeHtml(result.question || "")}</div>
    <div class="res-options">${rows}</div>
    <div class="res-meta">${n.toLocaleString()} synthetic residents</div>
    ${rationales.length ? `<div class="res-why">
      <div class="res-why-label">what people said</div>
      <ul>${rationales.map((r) => `<li>${escapeHtml(r)}</li>`).join("")}</ul>
    </div>` : ""}
    ${RESULT_ACTIONS}
  `;
  show(els.resultCard);
  wireResultActions();
}

// gentle "try rephrasing" card for an unsupported question (no map reveal)
function showRephrase(parsed, question) {
  state.phase = "results";
  setAsk("idle");
  els.progress.classList.remove("indeterminate");
  hide(els.summary);
  clearTimeout(toastTimer); hide(els.toast);
  map.clearVerdicts();
  cleanupBranch();

  const reason = parsed.reason || "I couldn't turn that into a poll for this city.";
  const examples = (parsed.examples || []).filter(Boolean).slice(0, 4);

  els.resultCard.innerHTML = `
    <div class="res-q">${escapeHtml(question || "")}</div>
    <div class="res-rephrase-label">try rephrasing</div>
    <div class="res-rephrase-reason">${escapeHtml(reason)}</div>
    ${examples.length ? `<div class="res-examples">
      ${examples.map((ex) => `<button type="button" class="res-example">${escapeHtml(ex)}</button>`).join("")}
    </div>` : ""}
    ${RESULT_ACTIONS}
  `;
  show(els.resultCard);
  // clicking an example pre-fills the composer with it, ready to submit
  els.resultCard.querySelectorAll(".res-example").forEach((btn) => {
    btn.addEventListener("click", () => {
      const text = btn.textContent;
      openInput();
      requestAnimationFrame(() => { els.askInput.value = text; els.askInput.focus(); autoGrow(); });
    });
  });
  wireResultActions();
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

// ── character inspector (tap a character when zoomed in) ────────────────────
const charOpen = () => !els.charCard.classList.contains("hidden");
function closeCharCard() { hide(els.charCard); }
const RACE_LABEL = { white: "white", black: "Black", asian: "Asian", hispanic: "Latino/Hispanic", pacific: "Pacific Islander", native: "Native American", other_multi: "multiracial" };
const EDUC_LABEL = { lt_hs: "no HS diploma", hs: "high-school educated", some_college: "some college", bachelors: "bachelor's degree", graduate: "graduate degree" };
const ISSUE_LABEL = { s_housing: "housing", s_crime: "public safety", s_homeless: "homelessness", s_cost: "cost of living", s_environment: "climate", s_immigration: "immigration" };
function leanLabel(v, lo, hi) { if (v == null) return null; return v < -0.33 ? lo : v > 0.33 ? hi : null; }
function topIssues(v, n = 2) {
  if (!v) return [];
  return Object.keys(ISSUE_LABEL).map((k) => [ISSUE_LABEL[k], v[k] ?? 0]).sort((a, b) => b[1] - a[1]).slice(0, n).map((x) => x[0]);
}
function showCharCard(s) {
  if (!s || !s.name) return;                 // offline-preview agents have no persona
  const v = s.values || {};
  const dem = [s.age != null ? `${s.age}` : null, RACE_LABEL[s.race] || s.race, EDUC_LABEL[s.educ] || s.educ].filter(Boolean).join(" · ");
  const tags = [leanLabel(v.economic, "economically left", "economically right"), leanLabel(v.social, "socially progressive", "socially conservative")].filter(Boolean);
  const issues = topIssues(v, 2);
  const isPoll = s.verdict != null;
  const label = isPoll ? `leaning ${s.verdict}` : "thinking";
  const labelClass = isPoll ? (s.verdict === "yes" ? "yes" : "no") : "";
  const thought = isPoll && s.rationale ? s.rationale : s.thought;
  els.charCard.innerHTML = `
    <button id="char-close" class="char-close" aria-label="Close">×</button>
    <div class="char-head">
      <canvas id="char-portrait" class="char-portrait" width="46" height="46"></canvas>
      <div class="char-id">
        <div class="char-name">${escapeHtml(s.name)}</div>
        <div class="char-sub">${escapeHtml(dem)}${s.hood ? " · " + escapeHtml(s.hood) : ""}</div>
      </div>
    </div>
    <div class="char-tags">
      ${tags.map((t) => `<span class="char-tag">${escapeHtml(t)}</span>`).join("")}
      ${issues.map((i) => `<span class="char-tag issue">cares about ${escapeHtml(i)}</span>`).join("")}
    </div>
    <div class="char-think">
      <div class="char-label ${labelClass}">${label}</div>
      <div class="char-thought">“${escapeHtml(thought || "…")}”</div>
    </div>`;
  show(els.charCard);
  $("char-close").addEventListener("click", closeCharCard);
  map.drawCharTo($("char-portrait"), s.char);
}
map.onSpriteTap = showCharCard;
map.onEmptyTap = () => { if (charOpen()) { closeCharCard(); return true; } return false; };

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
    else if (charOpen()) closeCharCard();
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
