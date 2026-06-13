# SF Digital Twin — Backend Build Brief

**Hand this file to Claude Code (`point at the repo, then /goal` against the rubric).**
This is the *backend* brief only. The frontend (8-bit SF map, sprites, speech bubbles, reactions) is a separate track; this backend must expose everything that track needs (see §10).

---

## 0. How to use this brief (orchestration — explicitly tuned for the Autonomy + Orchestration scoring)

**Wire all four loops below; the workflow scripts in §17 are themselves a judged orchestration artifact. Don't rely on a one-word trigger — early testing showed explicit step-by-step workflows beat them.**

- **Self-correction loop (`/goal`):** *Maximize the weighted score in `rubric.yaml`. Do not stop until (a) `cargo test` is green, (b) `cargo run --bin validate` exits 0 against the committed rubric, and (c) the deployed fly.io URL passes `/health` plus the endpoint-contract tests.* "Done" is machine-checkable; never wait for a human to confirm it. Read your own `validate` scorecard and pick the next experiment yourself.
- **Verifier + adversarial sub-agents:** before any milestone is "done", spawn a fresh agent (independent context) that re-runs `validate` + contract tests and grades against `rubric.yaml`; spawn a second **adversarial critic** that tries to prove the gain is spurious (overfit to one contest, model-knowledge leakage, weight-gaming). Gate completion on both — independent grading beats self-critique.
- **Dynamic workflows (§17):** push the loops bigger than one conversation into background JS workflow scripts — the hillclimb loop, parallel prompt/persona tuning, and the batch slide-runs. The script holds the loop, branching, and intermediate results so session context stays clean and holds only the final verified answer. **Save every working run as a rerunnable command** (`/sf:hillclimb`, etc.) — those saved commands are a judged artifact.
- **Continuous feedback wired in:** add a pre-push / CI hook running `cargo test` + a fast `validate --smoke`. The model should catch its own breakage from the check, not from a human pointing it out — that is exactly what the Autonomy score rewards.
- **Memory as the outer loop:** keep `NOTES.md` of failures → fixes → general rules (rate-limit handling, prompt patterns that aggregate well, snapshot pitfalls, leakage traps). The workflows append to it; distill mistakes into rules instead of re-deriving them.
- **Repeatable on a new problem:** the loop is problem-agnostic — swap `rubric.yaml` + the data source and the validate binary, workflows, verifier, and gates are unchanged. Say this in the submission; the Orchestration criterion explicitly asks whether another team could rerun the setup tomorrow on a new problem.
- **Build order is in §13.** Get the verifiable core (prediction engine + rubric) to green *before* layering the full life-sim, so the demo never hinges on the most fragile part.
- **Verify-at-build-time items are in §16.** Fetch them fresh (PUMS vintage, SF PUMA codes, market endpoints, fly.io deploy, Azure auth header). Pre-answering these in the brief is itself an autonomy lever — fewer mid-task stops to ask a human.

---

## 1. Problem, audience, definition of done

**Problem.** Produce a *distributionally accurate* synthetic population of San Francisco that can be polled and perturbed with events, and that yields population-level predictions: election/ballot-measure outcomes, issue/approval polling, prediction-market probabilities, and counterfactual opinion shifts.

**Who it's for.** Forecasters, campaigns, policy/product teams who want a queryable SF electorate they can run "what if this event happened" experiments against.

**Done looks like:**
1. `cargo test` green (contract, branching, weighting, marginals-match).
2. `cargo run --bin validate` exits 0: weighted rubric score ≥ thresholds in `rubric.yaml`.
3. Live fly.io URL: `/health` 200, all documented endpoints respond per contract.
4. `INTEGRATION.md` written so the frontend team can connect with zero backend questions.
5. A reproducible batch run at high N (5k–20k) whose outputs are saved (`runs/<id>/`) for the slides.

---

## 2. Core architecture — two engines, one persona layer

A **shared agent/persona layer** feeds two engines that must be runnable independently:

- **Prediction engine (the scored core).** `persona + as-of-date + event → opinion / vote / probability`, aggregated across agents with survey weights. One (batched) LLM call per question. Cheap, cacheable, deterministic in "clean" mode. This is what `validate` grades. It must run and pass the rubric *without the life-sim running at all.*
- **Life simulation (the visual demo).** Movement, daily routines, collocated conversations, reflection, birth/death on the SF map. Drives the frontend. LLM cost is bounded by tiering + batching (§5). Conversations can shift agent values and feed back into the prediction engine ("social mode", §4).

Decoupling is mandatory: the prediction engine is a hard dependency of the demo's *impact*; the life-sim is eye-candy + the "interesting" dynamics. Never let the life-sim be a single point of demo failure.

---

## 3. Agents and data

### 3.1 Sampling (the key idea: use real joint microdata)
- Sample agents from **ACS PUMS person microdata** for San Francisco County. PUMS records are already joint samples over age, sex, race/ethnicity, education, income, occupation/industry, household type, marital status, citizenship — so we get the real joint distribution for free instead of reconstructing it from marginals.
- **Carry the PUMS person weight (`PWGTP`) on every agent.** All population estimates use these weights (§4.3).
- **Religion** is not in census: layer it stochastically from public regional data (e.g., Pew Religious Landscape for the SF/Bay Area) conditioned on the agent's demographics.
- **Geography:** PUMS resolves to PUMA (SF County = a handful of PUMAs — fetch the current codes, §16). Assign a home location by drawing a valid residential cell within the agent's PUMA on the map grid; assign a work location from occupation + commute patterns. Finer-than-PUMA placement uses block-group marginals as a light reweight; do not over-engineer.
- All data must be **publicly available** (Census/IPUMS, Pew, OSM, SF Dept of Elections, public market APIs). No proprietary/unlicensed data or assets. PUMS is anonymized — no PII.

### 3.2 Persona generation ("core" / long-term memory)
For each sampled agent, generate a **seeded, deterministic** backstory from its demographics: schools, hobbies/interests, job history, core values/political-economic leanings, personality traits, media diet. Seed = `hash(simulation_seed, agent_index)` so runs are reproducible. Store a compact **value vector** (e.g., economic L/R, social L/R, trust-in-institutions, change-vs-status-quo, plus issue salience weights) alongside the prose persona — the value vector is what shifts under events and what aggregations and conversations read/write cheaply.

### 3.3 Actions (full set, see §5 for cost tiering)
Thinking, talking, pathfinding/moving, job-specific actions, eating/drinking/sleeping, leisure (music/reading/short-form/TV), marriage, plus birth/death processes. Each agent has an action **frequency** (default: ~30% of agents sampled per tick to act).

---

## 4. Prediction engine

### 4.1 Events
- **Broadcast events**: real/historical events (from news/search/social) or **user-provided** events (prompt, PDF, doc). Broadcasting delivers the event to agents who then update value vectors / opinions / actions.
- An event has: `text`, `as_of_date`, optional attachments, optional targeting (all agents vs a demographic filter).

### 4.2 Polling / querying agents
- A **poll** = `{question, options|free|binary, as_of_date, model, mode}`. The engine asks each agent (in batches, §5.4) to answer *in character, reasoning as of `as_of_date`*, returns a structured per-agent answer + short rationale.
- **As-of-date is first-class.** Agents must reason as of that date and not use later knowledge. For **counterfactuals**, default to the model with the **earliest knowledge cutoff (GPT-4o)** to limit outcome leakage; record which model answered.
- **Modes:**
  - `clean` (default for `validate`): agents answer from persona + broadcast events only, no social contamination → reproducible metrics.
  - `social`: answers reflect post-conversation value vectors from the life-sim → richer, used for exploration/demo, not for the graded headline number.

### 4.3 Aggregation math (make this explicit and tested)
For a binary/option question, with agent weights `w_i` (PUMS person weight, optionally × likely-voter propensity):

```
p_hat(option k) = Σ_i w_i · 1[answer_i = k]  /  Σ_i w_i
```

- Report `p_hat` per option, **per-demographic breakdowns** (by age band, race/ethnicity, education, income quintile, PUMA, etc.), and a **confidence interval** (weighted; design-effect-aware bootstrap is fine).
- **Elections/turnout:** restrict to the **citizen voting-age** subset (PUMS citizenship + age), then apply a **likely-voter propensity by demographic** before weighting. Turnout model is configurable; document it.
- **Markets:** map a market question to a pollable question; `p_hat(yes)` is the sim's probability. Bucket each market as `sf_opinion_informative` or `general_knowledge` (§11) — report both buckets separately; the headline market number weights the informative bucket.

---

## 5. Life simulation (bounded-cost generative agents)

> Reference architecture: Stanford "Generative Agents" (memory stream + retrieval + reflection + planning). That paper ran 25 agents; scaling to thousands requires the cost controls below. **These are load-bearing, not optional.**

### 5.1 Map
- Consume `tiles.db` (prepared by the map track): `chunks` table = zstd-compressed `u8` cost array per chunk/LOD; `buildings` table = per-chunk footprints + tier. Grid 67×60 chunks × 250m, 125×125 cells at 2m/cell (LOD 0). UTM Zone 10N (EPSG:32610), bbox `(541826, 4172448)–(558483, 4187294)`.
- Cost values: `0` free (roads/sidewalks/plazas), `1` park/shoreline penalty, `2` grass penalty, `8` stairs, `255` blocked (water/buildings/cliffs).

### 5.2 Pathfinding — deterministic, never LLM
- A\* (or flow-field for shared destinations) over the cost grid. The **LLM picks intent** ("go to work / cafe / home"); **code computes the path**. No LLM call per step. Cache flow fields to common destinations.

### 5.3 Routine actions — schedule-driven
- Eating/sleeping/commuting/leisure run off a per-agent daily schedule derived from job + persona; mostly rule-based. LLM flavor only occasionally and only for memory-worthy moments.

### 5.4 LLM-cost tiering + batching (the scaling lever)
LLM calls concentrate on: (1) **event reactions**, (2) **collocated conversations/debates**, (3) **periodic reflection**, (4) **polling**. Everything else is code.
- **Batched decisions:** one structured call returns a JSON array of decisions for ~10–20 agents grouped by **locality** (same chunk) or **archetype** (similar persona cluster). Target: turn ~6k naive calls/tick into a few hundred.
- **Persona-cluster + event caching:** agents in the same cluster reacting to the same event reuse a cached reasoning template with light per-agent variation. Cache key = `(cluster_id, event_id, as_of_date, model)`.
- **Bounded conversation rate:** cap conversations per tick; only collocated pairs/groups with sufficient value-distance get a debate (which can shift value vectors); others get a no-op chat that doesn't touch memory.

### 5.5 Birth/death
- Stochastic processes at demographically plausible rates; births create new agents (inherit + perturb local distribution), deaths remove agents and their sprites. Keep weights re-normalized.

---

## 6. Memory (generative-agent layer)

- **Core memory:** persona identity + value vector (§3.2). Mostly stable; values shift slowly under events/debates.
- **Episodic memory stream:** timestamped observations/events/conversations. Retrieval by `recency × importance × relevance`. To bound cost, start with recency + importance + tag/keyword relevance; **embeddings optional** via a local model (e.g., `fastembed`/`candle`) rather than per-memory API calls.
- **Reflection:** infrequent synthesis of recent stream into higher-level beliefs that can update the value vector. Cap stream length per agent; summarize-and-truncate.

---

## 7. State model and branching (git-style)

- **Static vs mutable split** (critical for snapshot size at 20k):
  - *Static* (stored once per simulation, referenced by snapshots): persona prose, seed, home/work cells, base demographics + weight.
  - *Mutable* (snapshotted): position, current action, value vector, recent memory delta, relationship edges, alive/dead, clock.
- **Snapshots:** serialize mutable state (serde) into a `snapshots` table in `state.db` (sqlite) with `parent_id`, `tick`, `label`, `state_hash`. Static layer stored once → snapshots stay small.
- **Main branch:** linear chain; commit every `n` ticks (configurable).
- **Branch:** `from snapshot_id → clone mutable state into new branch → apply event → run k ticks`. Fully queryable while running.
- **Reset:** repoint the active session to `main` HEAD; branch is droppable.
- **Tests (must pass):** branching does not mutate main; reset restores the exact `state_hash`; commit/restore round-trips bit-for-bit on mutable state.

---

## 8. Model client (Azure AI Foundry, raw HTTP)

- **Do not use the OpenAI Rust SDK** — Azure's `/responses` shape won't match cleanly. Use `reqwest` with two code paths.
- **Base URL (all three models):** `https://claude-day-resource.services.ai.azure.com/openai/v1`
- **Auth:** read `MODEL_API_KEY` from env; send `Authorization: Bearer $MODEL_API_KEY`. If `401`, fall back to header `api-key: $MODEL_API_KEY` (verify, §16). **Never hardcode/commit the key; `.gitignore` the `.env`.**
- **Shapes:**
  - **Grok-4.3** → `POST {base}/chat/completions`, body `{model:"grok-4.3", messages:[...]}`, read `choices[0].message.content`.
  - **GPT-4o** → `POST {base}/responses`, body `{model:"gpt-4o", input:"..."}`, read `output[0].content[0].text`. **`max_output_tokens` must be ≥ 16.**
  - **GPT-5.5** → same `/responses` shape, `model:"gpt-5.5"`.
- **Model roles:** GPT-4o = counterfactuals (earliest cutoff) + cheap bulk decisions; GPT-5.5 = higher-stakes reactions/reflection; Grok-4.3 = diversify / load-balance. Selectable per call; default configurable.
- **Concurrency + resilience (Azure rate limits are the real bottleneck):** a single client with a **semaphore-bounded** in-flight cap, **exponential backoff + jitter on 429/5xx**, request timeouts, and a token/RPM budget. Fan across the three deployments to raise effective throughput. Structured-output calls request strict JSON; parse defensively (strip code fences).

---

## 9. API surface (axum)

- `POST /simulations` — create a seeded sim: `{n, seed, start_datetime, tick_seconds, commit_every, distributional_params?}` → `simulation_id`. `distributional_params` lets a caller seed mean/std or other params per demographic.
- `GET /simulations/{id}/demographics` — empirical marginals across agents **+ comparison to target ACS marginals** (per-variable distance + pass/fail vs tolerance).
- `POST /simulations/{id}/branches` — `{event?, ticks, model?, mode?}` → `branch_id`; creates a branch and simulates it.
- `GET /branches/{id}` — status, clock, tick.
- `GET /branches/{id}/agents?filter=...` — subset of agents with current action, position, recent reaction; filter by any demographic.
- `POST /branches/{id}/poll` — `{question, options|binary, as_of_date, model, mode}` → weighted distribution + per-demographic breakdown + CI.
- `POST /branches/{id}/predict-market` — `{market_id|question, as_of_date}` → sim probability + bucket label + (if open) live market price.
- `POST /simulations/{id}/reset-to-main` (and/or `DELETE /branches/{id}`) — revert to main HEAD, continue.
- `GET /health` — liveness + model-client reachability.
- Streaming: `GET /branches/{id}/stream` (SSE; WS acceptable) — see §10.

All endpoints have **contract tests** runnable against the deployed URL.

---

## 10. Frontend integration (must be ready for the map/sprite track)

The frontend will render hundreds–thousands of sprites moving as their agents, with per-agent reactions (👍/👎) and speech bubbles. Backend must provide:
- **Snapshot fetch** of agent positions + current action for a branch/tick (paginated; supports thousands).
- **Stream** (`/branches/{id}/stream`, SSE) emitting typed events: `agent_moved {id, from_cell, to_cell, path?}`, `agent_said {id, text, target_id?}` (speech bubbles), `agent_reacted {id, kind: up|down, event_id}`, `tick {clock}`, `birth`/`death`.
- **Coordinate contract:** document the cell/chunk ↔ UTM ↔ lat/long mapping and how positions index into the `tiles.db` grid, so sprites land correctly.
- Write all of this in **`INTEGRATION.md`**: base URL, auth (if any), every endpoint with example request/response, the SSE event schema, the coordinate contract, and a minimal "connect in 5 minutes" snippet.

---

## 11. Validation and rubric (the model-checkable "done")

- **`rubric.yaml`** (starter provided separately) defines targets + tolerances + weights across categories: `elections_measures`, `resolved_markets` (bucketed `sf_opinion_informative` vs `general_knowledge`), `live_markets` (demo-only agreement, not truth), `counterfactuals`.
- **`cargo run --bin validate`**: spins a sim at a validation N, runs the polls/market mappings for each rubric entry in `clean` mode at the specified `as_of_date`/model, computes metrics, prints a scorecard, and **exits 0 only if the weighted score ≥ thresholds**.
- **Metrics:** elections/measures → absolute error on vote share + pass/fail vs tolerance; markets → **Brier score vs resolved outcome** (preferred) and/or agreement vs live price; counterfactuals → correct **direction** + plausible **magnitude**, compared to a real before/after poll where one exists.
- **Weighting:** markets weighted higher per the team's call, but the headline market score weights the `sf_opinion_informative` bucket; the `general_knowledge` bucket is reported separately and never inflates the headline (the panel is a weak instrument there).
- **Verifier sub-agent** grades the scorecard against the rubric in an independent context before any milestone is marked done.

---

## 12. Scaling and cost budget

- Naive: `N × 0.30 × 1 call/tick`. At `N=20k`, 30s ticks ⇒ ~200 calls/s — infeasible live. With deterministic pathfinding + schedule-driven routines + batched decisions (10–20×) + cluster/event caching, live `N≈1–2k` is watchable and `N=5k–20k` is a **batch run** for slides.
- Two run profiles: **demo** (small N, real-time ticks, `social` mode for flavor) and **batch** (large N, fast ticks, outputs saved to `runs/<id>/` with the scorecard for the slides). Both configurable from one binary.
- Track and log tokens/calls per tick; surface a cost estimate so the team can size the slide runs against the Azure credit pool.

---

## 13. Build order (each milestone gates on its own green check)

1. **Scaffold + secrets + model client.** Repo, `.env`/`.gitignore`, the two-shape `reqwest` client with semaphore + retry. *Gate:* unit tests hit a tiny live call per model shape (mocked + one real smoke test).
2. **Agent sampling + personas.** PUMS ingest, seeded persona + value vector, religion layer, home/work assignment. *Gate:* `GET /demographics` shows sampled marginals match ACS targets within tolerance (test).
3. **Prediction engine + rubric + validate.** Poll/event/aggregation with weights + as-of-date + clean mode; `rubric.yaml`; `validate` binary. *Gate:* hill-climb until `validate` exits 0.
4. **State + branching.** Static/mutable split, snapshots, branch, reset. *Gate:* branch-isolation + reset-roundtrip + hash tests green.
5. **Life-sim core.** Load `tiles.db`, A\* pathfinding, schedules, positions endpoint + SSE stream. *Gate:* a small sim runs ticks, sprites move on valid cells, stream emits typed events; contract test on `/stream`.
6. **Generative layer.** Memory stream + retrieval + reflection + batched collocated conversations that shift value vectors; `social` mode feeds the prediction engine. *Gate:* `social` vs `clean` poll divergence is sane; cost stays within budget.
7. **Deploy + integrate.** fly.io, `/health`, `INTEGRATION.md`, saved batch runs. *Gate:* deployed URL passes `/health` + contract tests; verifier signs off on the full rubric.

---

## 14. Deploy (fly.io)

- Containerize the Rust service; expose the API + SSE. Set `MODEL_API_KEY` as a fly **secret** (not in the image). `/health` checks liveness + model reachability. Document config (N, tick rate, commit interval, model roles) via env/flags. Verify current fly.io Rust deploy specifics at build time (§16).

---

## 15. Constraints (banned-project rules)

- Public/own-created data and assets only (Census/IPUMS, Pew, OSM, SF Dept of Elections, public market APIs). No proprietary or unlicensed data/assets.
- No PII; PUMS is anonymized synthetic-ish microdata. Personas are fabricated.
- Secrets never committed; `.env` git-ignored; key only from env / fly secret.

---

## 16. Verify-at-build-time (fetch fresh; do not assume)

1. **ACS PUMS** current 1-yr/5-yr vintage + the download endpoint, and the **PUMA codes for San Francisco County**.
2. **Pew** regional religious-composition figures for the SF/Bay Area.
3. **Polymarket** public API (markets + resolution) and **Kalshi** API (needs an account/creds — optional if unavailable); pick a curated set per bucket.
4. **SF Dept of Elections / CA SOS** ground-truth results for the elections/measures in the rubric (precinct/county level).
5. **fly.io** current Rust deployment flow + secrets.
6. **Azure auth header**: confirm `Authorization: Bearer` vs `api-key:` against the live endpoint with a smoke call.
7. Confirm model knowledge-cutoffs (GPT-4o earliest) to set counterfactual `as_of_date` windows that minimize leakage.

---

## 17. Dynamic workflows (background orchestration scripts)

Implement these as Claude Code **dynamic workflows** — JS scripts the runtime executes in the background while the session stays free. Use your current dynamic-workflow runtime API; the specs below fix *what fans out, when verification runs, and what gates completion* — make that explicit in each script. Keep deterministic metric math in the Rust `validate` binary (fast, reproducible); the workflows orchestrate the *agentic, parallel* work around it. **Save each working run as a named command** so it's rerunnable and an artifact for the judges.

### 17.1 `hillclimb` — outer self-correction loop (centerpiece)
- **Command:** `/sf:hillclimb [--max-iter N] [--budget tokens]`
- **Step 1 (gate):** run `cargo run --bin validate`; parse the scorecard. Weighted score ≥ threshold → go to Step 5.
- **Step 2 (diagnose):** rank failing rubric entries by weighted deficit.
- **Step 3 (fan-out):** spawn one **fix sub-agent per failing entry** in an isolated context; each may only touch prompt templates, persona generation, aggregation/weighting, or the turnout model — never the rubric targets or the held-out slice. Barrier until all return.
- **Step 4 (verify gate):** re-run `validate`; spawn the **verifier** (re-grades) and the **adversarial critic** (checks the gain isn't overfit / leakage / weight-gaming). If either rejects a change, revert it and log why in `NOTES.md`. Loop to Step 1 until pass or `--max-iter`/budget reached.
- **Step 5 (distill):** append general rules to `NOTES.md`; write the final scorecard to `runs/hillclimb-<ts>/`.
- **Completion gate:** weighted score ≥ threshold AND verifier + critic both pass.

### 17.2 `tune-prompts` — parallel experiment with an overfit/leakage guard
- **Command:** `/sf:tune-prompts --variants K --target <persona|poll|reaction>`
- **Fan-out:** K variants of the chosen template, each run by a sub-agent against a **training slice** of the rubric only.
- **Selection:** rank on the training slice, then re-score the top few on a **held-out slice** tuning never saw; promote the winner only if it also wins held-out (guards against overfitting to one contest and against baking in model-knowledge leakage).
- **Completion gate:** winner beats the incumbent on held-out, else keep the incumbent. Log all variants + scores to `runs/tune-<ts>/`.

### 17.3 `batch-runs` — slide artifacts at scale, in the background
- **Command:** `/sf:batch-runs --configs configs/*.toml`
- **Fan-out:** one large-N (5k–20k) simulation per config (varying N / seed / model-role / event set), run concurrently within the Azure rate budget while the session stays free.
- **Verification:** each run ends by invoking `validate` on its outputs and writing a scorecard.
- **Completion gate:** every config produced `runs/<id>/` with snapshot refs, a scorecard, and a token/cost log. These saved runs are the impact evidence for the demo deck.

### 17.4 Scoring map (state this in the submission)
- **Autonomy (15%):** the `/goal` "do not stop until" gate + CI hook + verifier/critic mean breakage is caught by checks, not humans; the workflows run long unsupervised stretches.
- **Orchestration (15%):** `BRIEF.md` + `rubric.yaml` + the saved `/sf:*` commands are simple, repeatable, and make "done" verifiable without a human (tests, responding URL, gradable rubric); swap the rubric + data source to rerun on a new problem tomorrow.
