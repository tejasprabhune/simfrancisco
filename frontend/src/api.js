// ─────────────────────────────────────────────────────────────────────────
// Thin client over the SF Digital Twin backend (see INTEGRATION.md).
// Every call has a timeout and throws a readable Error on non-2xx.
// ─────────────────────────────────────────────────────────────────────────

import { BASE, SIM, PREDICT } from "./config.js";

async function req(path, { method = "GET", body, timeout = 30000, signal } = {}) {
  const ctrl = new AbortController();
  const t = setTimeout(() => ctrl.abort(), timeout);
  // honor an external abort signal (user cancellation) in addition to the timeout
  if (signal) {
    if (signal.aborted) ctrl.abort();
    else signal.addEventListener("abort", () => ctrl.abort(), { once: true });
  }
  try {
    const res = await fetch(`${BASE}${path}`, {
      method,
      headers: body ? { "content-type": "application/json" } : undefined,
      body: body ? JSON.stringify(body) : undefined,
      signal: ctrl.signal,
    });
    const text = await res.text();
    let data;
    try { data = text ? JSON.parse(text) : {}; } catch { data = { raw: text }; }
    if (!res.ok) {
      const msg = data?.error || data?.message || text || res.statusText;
      throw new Error(`${method} ${path} → ${res.status}: ${msg}`);
    }
    return data;
  } catch (e) {
    if (e.name === "AbortError") throw new Error(`${method} ${path} timed out`);
    throw e;
  } finally {
    clearTimeout(t);
  }
}

export const health = () => req("/health", { timeout: 8000 });

export const createSimulation = (overrides = {}) =>
  req("/simulations", { method: "POST", body: { ...SIM, ...overrides }, timeout: 60000 });

// Page through every alive agent on a branch. Returns [{id, name, lonlat, values, ...}].
// `cap` is only a runaway guard; the real bound is the branch's total_matched,
// which we learn from the first page — so we never silently drop agents.
export async function getAllAgents(branchId, onProgress, cap = 50000) {
  const limit = 1000;
  let offset = 0;
  const out = [];
  while (offset < cap) {
    const page = await req(`/branches/${encodeURIComponent(branchId)}/agents?limit=${limit}&offset=${offset}`, { timeout: 30000 });
    const batch = page.agents || [];
    out.push(...batch);
    const total = page.total_matched ?? out.length;
    offset += limit;
    if (onProgress) onProgress(out.length, total);
    if (out.length >= total || batch.length < limit) break;
  }
  return out;
}

// "Trigger the branching": clone main, optionally broadcast an event, run a
// couple ticks so the branch reflects it.
export const createBranch = (simId, { ticks = PREDICT.branch_ticks, event, name, signal } = {}) =>
  req(`/simulations/${encodeURIComponent(simId)}/branches`, {
    method: "POST",
    body: { ticks, ...(event ? { event } : {}), ...(name ? { name } : {}) },
    timeout: 90000,
    signal,
  });

// Poll the synthetic electorate. The LLM pass takes ~5-15s.
export const poll = (branchId, payload, signal) =>
  req(`/branches/${encodeURIComponent(branchId)}/poll`, {
    method: "POST",
    body: payload,
    timeout: 180000,
    signal,
  });

export const deleteBranch = (branchId) =>
  req(`/branches/${encodeURIComponent(branchId)}`, { method: "DELETE", timeout: 15000 })
    .catch(() => {}); // best-effort cleanup
