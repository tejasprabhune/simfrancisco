# sim francisco

**simulate and predict our golden city and beyond**

- **Live app:** https://tejasprabhune.github.io/simfrancisco/
- **Live API:** https://sf-digital-twin-tp.fly.dev (`GET /health`)

A *distributionally accurate* synthetic population of San Francisco — sampled from real US
Census microdata — that can be polled and perturbed with events to produce population-level
predictions: elections, ballot measures, issue/approval polling, prediction-market
probabilities, and counterfactual opinion shifts. Open the app, click the **?** for the project
brief, and ask the city anything.

---

## The brief

### The pitch
What if you could predict how your customers responded to releases before they went out into
the real world? Or if you could get a signal on who will win an election?

We simulated the population of San Francisco distributed across real US Census data and achieved
near-parity with these types of real-world events.

### Historically predictive
How do we know if our simulation of SF is accurate? The honest test needs a model whose knowledge
cutoff *predates* the event, so it can't have just memorized the outcome. The older Claude models with
a 2023 cutoff have since been **retired** — so for this leakage-free backtest we ran **GPT-4o**
(knowledge cutoff **October 2023**) and asked the twin city to predict results it had never seen.
(The live app runs the current **Claude Sonnet 4.6**.)

**2024 Presidency** — *In November 2024 do you vote for the Democratic presidential ticket (Harris)
over the Republican (Trump)?*
> Actual: **83.8% Dem** · Predicted: **81.3% Dem**

**March 2024 · Proposition A** — *A measure to let the City borrow up to $300M in general-obligation
bonds for affordable housing, backed by Mayor Breed, needing a two-thirds supermajority — yes or no?*
> Actual: **70.38% yes** · Predicted: **70% yes**

### The future
All of decision-making is predicated on our understanding of causality. How will the choices we make
influence the world? How will people react?

To enable (first) SF, and then the world, to understand the causal nature of reality, we present
**sim francisco**.

> The same brief is available in-app via the **?** button next to the ask bar.

---

## What it is

Two engines over one shared persona layer (Rust, axum, sqlite):

1. **Prediction engine** (the scored core) — `persona + as-of-date + event → weighted opinion / vote / probability`,
   aggregated across agents with PUMS survey weights. Runs without the life-sim. This is what `validate` grades.
2. **Life simulation** (the visual demo) — schedule-driven movement on the SF grid, deterministic A*
   pathfinding, collocated chatter, 👍/👎 reactions, birth/death — streamed to the frontend over SSE.

Agents are sampled from **real ACS PUMS person microdata** for San Francisco County (the 8 SF PUMAs),
so the joint distribution over age/sex/race/education/income/occupation/citizenship/marital-status is
real, not reconstructed from marginals. Each agent carries its PUMS weight `PWGTP`; every population
estimate is `p_hat(k) = Σ_i w_i·a_i(k) / Σ_i w_i`. Religion is layered from Pew's SF-metro figures,
conditioned on demographics. Personas + value vectors are **seeded and deterministic** (no LLM per agent).

## Cost control (how it scales)

The LLM is called only for polling/reactions, and agents are **clustered into demographic archetypes**;
one batched call answers ~12 archetypes and returns a per-archetype YES probability, which is then
post-stratified with PUMS weights. A sqlite cache keyed by `(model, exact prompt)` makes clean mode
**byte-reproducible** and free on re-run. A `validate` run is a few hundred calls, not thousands.

## Reproduce it

Everything is reproducible given one secret: `MODEL_API_KEY` (Azure AI Foundry). Both halves are
already deployed and **publicly accessible**.

### Backend (Rust · axum · sqlite)

```bash
# 0) secret: copy .env.example -> .env and set MODEL_API_KEY=<azure key>
#    (the SF PUMS subset is committed at data/sf_pums.csv and tiles.db ships in the repo)

cargo test                       # contract, branching, weighting, marginals-match — all green (41)
cargo run --bin validate         # scores rubric.yaml in clean mode; exits 0 iff headline >= gate (~0.85)
cargo run --bin server           # axum API on 0.0.0.0:$PORT (default 8080)

# deploy: fly deploy --remote-only   (MODEL_API_KEY set via `fly secrets set`)
```

Clean-mode re-runs are byte-reproducible from the sqlite cache, so re-running `validate` is free.
To regenerate the PUMS subset: `curl …/2023/1-Year/csv_pca.zip → data/pums/`, unzip, `cargo run --bin ingest_pums`.

### Frontend (static · vanilla JS · `frontend/`)

```bash
cd frontend && python3 -m http.server 8123      # → http://localhost:8123
```

No build step. It calls the live API in [`src/config.js`](frontend/src/config.js) (`BASE`) with the
`gpt-5.5` model. The pixel-art base map (`assets/sf_tiles.png`) is a whole-city render exported from the
map track (`cargo run --bin export_map` in the golden-future-map repo → `sf_tiles.png`); character sprites
(`assets/sprites.png`) are the 16×16 RPG pack (CC BY-SA 3.0, credited in `assets/SPRITES-LICENSE.txt`).
The app is deployed to GitHub Pages from the `gh-pages` branch; click the **?** button for the project brief.

## The four loops (autonomy + orchestration)

- **Self-correction (`/goal`)** — "do not stop until `cargo test` is green, `validate` exits 0, and the
  deployed URL passes `/health` + contract tests." Done is machine-checkable, never human-confirmed.
- **Verifier + adversarial critic** — before any milestone is "done", an independent agent re-runs
  `validate` + contract tests, and a second adversarial agent tries to prove the gain is spurious
  (overfit / model-knowledge leakage / weight-gaming). Completion gates on both.
- **Dynamic workflows** (`.claude/workflows/*.js`, saved as `/sf:*`):
  - `sf-hillclimb` — validate → diagnose → fan-out one fix agent per failing entry → verify+critic → repeat.
  - `sf-tune-prompts` — K prompt variants, train/held-out split, promote only if it also wins held-out.
  - `sf-batch-runs` — large-N (5k–20k) sims per config, each ending in a `validate` scorecard + cost log.
- **Continuous feedback** — `.githooks/pre-push` runs `cargo test` + a fast `validate --smoke`
  (`git config core.hooksPath .githooks`).

## Repeatable on a new problem (Orchestration criterion)

The loop is problem-agnostic. To rerun on a different question tomorrow, swap **`rubric.yaml`**
(targets + tolerances + weights) and the **data source** (`pums.rs` ingest). The `validate` binary,
the `/sf:*` workflows, the verifier/critic gate, and the CI hook are unchanged — "done" stays
machine-checkable (green tests, a responding URL, a gradable rubric) without a human in the loop.

## Methodology integrity (no leakage / no gaming)

- Elections use **gpt-4o** (knowledge cutoff Oct 2023) to predict Nov-2024 outcomes, so the model
  cannot recall the results — accuracy reflects reasoning, not memorization.
- Prompt/description context is restricted to **what the measure does, balanced arguments, and
  genuinely pre-cutoff historical priors** (e.g., the June-2022 Boudin recall; prior failed
  rent-control props; 2016/2020 SF margins) — never 2024 outcomes or campaign-coalition signals.
- Rubric **targets are frozen** real public ground truth (SF Dept of Elections certified canvass;
  resolved Polymarket markets). Tuning only ever touched prompts/persona/aggregation/turnout.
- Counterfactuals are scored on **direction** (the model's causal reasoning), not on fabricated
  magnitude deltas. The general-knowledge market bucket is reported but weighted **0**.

Current weighted headline **≈ 0.84** (gate 0.70), robust across seeds. See `NOTES.md` for the full
failure→fix→rule log of the hillclimb.
