//! `validate` — the machine-checkable "done".
//!
//! Loads rubric.yaml, builds a population at validation_n, runs each entry's poll in
//! CLEAN mode at its as_of_date/model, computes the metric, prints a scorecard, and
//! exits 0 iff the weighted score ≥ thresholds.weighted_score_min. Targets are fixed
//! public ground truth; only persona/prompt/aggregation may be tuned (never targets).

use simfrancisco::model::{Cache, ModelClient};
use simfrancisco::persona::build_population;
use simfrancisco::predict::{Engine, Event, Framing, Poll};
use simfrancisco::pums;
use simfrancisco::rubric::*;
use std::sync::Arc;

#[derive(Default)]
struct Args {
    smoke: bool,
    rubric: String,
    n: Option<usize>,
    seed: Option<u64>,
    out: Option<String>,
    quiet: bool,
}

fn parse_args() -> Args {
    let mut a = Args { rubric: "rubric.yaml".into(), ..Default::default() };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--smoke" => a.smoke = true,
            "--quiet" => a.quiet = true,
            "--rubric" => a.rubric = it.next().unwrap_or(a.rubric.clone()),
            "--n" => a.n = it.next().and_then(|v| v.parse().ok()),
            "--seed" => a.seed = it.next().and_then(|v| v.parse().ok()),
            "--out" => a.out = it.next(),
            _ => {}
        }
    }
    a
}

#[tokio::main]
async fn main() {
    simfrancisco::load_dotenv(".env");
    let args = parse_args();
    let code = run(args).await;
    std::process::exit(code);
}

async fn run(args: Args) -> i32 {
    let rubric = match Rubric::load(&args.rubric) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to load rubric {}: {e}", args.rubric);
            return 2;
        }
    };
    let seed = args.seed.unwrap_or(rubric.meta.default_seed);
    let mut n = args.n.unwrap_or(rubric.meta.validation_n);
    if args.smoke {
        n = n.min(400);
    }

    let records = match pums::load_sf() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to load SF PUMS ({e}). Run `cargo run --bin ingest_pums` first.");
            return 2;
        }
    };
    let cache = Cache::open("cache.db").ok().map(Arc::new);
    let client = match ModelClient::from_env(cache) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("model client error: {e}");
            return 2;
        }
    };
    if !client.has_key() && std::env::var("MODEL_OFFLINE").is_err() {
        eprintln!("WARNING: MODEL_API_KEY not set; only cached results will resolve.");
    }
    let engine = Engine::new(client.clone());

    eprintln!("Building population N={n} seed={seed} (clean mode) ...");
    let pop = build_population(&records, n, seed, None);

    let mut categories: Vec<CategoryScore> = Vec::new();
    let mut report = serde_json::Map::new();

    // ---- elections & measures ----
    let mut e_scores = Vec::new();
    let mut e_rows = Vec::new();
    let mut e_max_err = 0.0f64;
    for e in &rubric.elections_measures {
        let poll = Poll {
            question: e.question.clone(),
            description: e.description.clone(),
            framing: Framing::Vote,
            as_of_date: e.as_of_date.clone(),
            model: Some(e.model.clone()),
            population: Some(e.population.clone()),
            event: None,
        };
        let res = match engine.run_poll(&pop, &poll).await {
            Ok(r) => r,
            Err(err) => {
                eprintln!("  [{}] poll error: {err}", e.id);
                continue;
            }
        };
        let (score, abs_err, pass) = election_entry_score(res.p_yes, e.target_share, e.tolerance);
        e_max_err = e_max_err.max(abs_err);
        e_scores.push(score);
        if !args.quiet {
            println!(
                "  ELECTION {:<34} pred={:.3} target={:.3} err={:.3} tol={:.3} score={:.2} {}",
                e.id, res.p_yes, e.target_share, abs_err, e.tolerance, score, pass_mark(pass)
            );
        }
        e_rows.push(serde_json::json!({
            "id": e.id, "pred": res.p_yes, "target": e.target_share, "abs_err": abs_err,
            "tolerance": e.tolerance, "score": score, "pass": pass, "ci": [res.ci_low, res.ci_high],
            "n_agents": res.n_agents, "n_eff": res.n_eff, "n_archetypes": res.n_archetypes,
        }));
    }
    let e_cat = mean(&e_scores);
    let e_pass = e_max_err <= rubric.thresholds.elections_measures_max_abs_err;
    categories.push(CategoryScore { name: "elections_measures".into(), score: e_cat, weight: rubric.weights.elections_measures, n: e_scores.len(), passed: e_pass });
    report.insert("elections_measures".into(), serde_json::json!({"entries": e_rows, "category_score": e_cat, "max_abs_err": e_max_err, "max_abs_err_threshold": rubric.thresholds.elections_measures_max_abs_err}));

    // ---- resolved markets: informative (scored) ----
    let mut m_scores = Vec::new();
    let mut m_rows = Vec::new();
    let mut m_max_brier = 0.0f64;
    for m in &rubric.resolved_markets.sf_opinion_informative {
        let poll = market_poll(m);
        let res = match engine.run_poll(&pop, &poll).await {
            Ok(r) => r,
            Err(err) => { eprintln!("  [{}] market error: {err}", m.id); continue; }
        };
        let (score, b, pass) = market_entry_score(res.p_yes, m.outcome, rubric.thresholds.resolved_markets_max_brier);
        m_max_brier = m_max_brier.max(b);
        m_scores.push(score);
        if !args.quiet {
            println!(
                "  MARKET   {:<34} pred={:.3} outcome={:.0} brier={:.3} score={:.2} {}",
                m.id, res.p_yes, m.outcome, b, score, pass_mark(pass)
            );
        }
        m_rows.push(serde_json::json!({"id": m.id, "pred": res.p_yes, "outcome": m.outcome, "brier": b, "score": score, "pass": pass}));
    }
    let m_cat = mean(&m_scores);
    let m_pass = m_max_brier <= rubric.thresholds.resolved_markets_max_brier || m_scores.is_empty();
    categories.push(CategoryScore { name: "resolved_markets_sf_informative".into(), score: m_cat, weight: rubric.weights.resolved_markets_sf_informative, n: m_scores.len(), passed: m_pass });

    // general-knowledge bucket: reported only, weight 0
    let mut g_rows = Vec::new();
    for m in &rubric.resolved_markets.general_knowledge {
        let poll = market_poll(m);
        if let Ok(res) = engine.run_poll(&pop, &poll).await {
            let b = brier(res.p_yes, m.outcome);
            if !args.quiet {
                println!("  (general) {:<33} pred={:.3} outcome={:.0} brier={:.3}  [reported, not scored]", m.id, res.p_yes, m.outcome, b);
            }
            g_rows.push(serde_json::json!({"id": m.id, "pred": res.p_yes, "outcome": m.outcome, "brier": b}));
        }
    }
    report.insert("resolved_markets".into(), serde_json::json!({"sf_informative": m_rows, "category_score": m_cat, "max_brier": m_max_brier, "general_knowledge_reported": g_rows}));

    // ---- counterfactuals ----
    let mut c_scores = Vec::new();
    let mut c_rows = Vec::new();
    let mut c_dir_ok = 0usize;
    for c in &rubric.counterfactuals {
        let base = Poll {
            question: c.question.clone(),
            description: c.description.clone(),
            framing: if c.framing == "belief" { Framing::Belief } else { Framing::Vote },
            as_of_date: c.as_of_date.clone(),
            model: Some(c.model.clone()),
            population: c.population.clone(),
            event: None,
        };
        let ev = Event { text: c.event.clone(), as_of_date: c.as_of_date.clone() };
        let (b0, b1, delta) = match engine.run_counterfactual(&pop, &base, ev).await {
            Ok(x) => x,
            Err(err) => { eprintln!("  [{}] cf error: {err}", c.id); continue; }
        };
        let up = c.expected_direction.eq_ignore_ascii_case("up");
        let (score, dir_ok) = cf_entry_score(delta, up, c.real_poll_delta, c.magnitude_tolerance.unwrap_or(0.1));
        if dir_ok { c_dir_ok += 1; }
        c_scores.push(score);
        if !args.quiet {
            println!(
                "  CF       {:<34} base={:.3} after={:.3} Δ={:+.3} expect={} score={:.2} {}",
                c.id, b0.p_yes, b1.p_yes, delta, c.expected_direction, score, pass_mark(dir_ok)
            );
        }
        c_rows.push(serde_json::json!({"id": c.id, "baseline": b0.p_yes, "after": b1.p_yes, "delta": delta, "expected": c.expected_direction, "direction_ok": dir_ok, "score": score}));
    }
    let c_cat = mean(&c_scores);
    let c_frac_dir = if c_scores.is_empty() { 1.0 } else { c_dir_ok as f64 / c_scores.len() as f64 };
    let c_pass = c_frac_dir >= rubric.thresholds.counterfactual_direction_min || c_scores.is_empty();
    categories.push(CategoryScore { name: "counterfactuals".into(), score: c_cat, weight: rubric.weights.counterfactuals, n: c_scores.len(), passed: c_pass });
    report.insert("counterfactuals".into(), serde_json::json!({"entries": c_rows, "category_score": c_cat, "direction_correct_frac": c_frac_dir, "direction_min": rubric.thresholds.counterfactual_direction_min}));

    // ---- headline ----
    let headline = weighted_headline(&categories);
    let gate = rubric.thresholds.weighted_score_min;
    let passed = headline >= gate;

    println!("\n================ SCORECARD ================");
    for c in &categories {
        println!(
            "  {:<34} score={:.3} weight={:.1} n={} {}",
            c.name, c.score, c.weight, c.n, pass_mark(c.passed)
        );
    }
    let usage = client.usage.snapshot();
    println!("  {:-<42}", "");
    println!("  WEIGHTED HEADLINE = {:.4}   (gate ≥ {:.2})  {}", headline, gate, pass_mark(passed));
    println!("  sub-thresholds: elections_max_abs_err={:.3} markets_max_brier={:.3} cf_direction={:.2}", e_max_err, m_max_brier, c_frac_dir);
    println!("  llm: {} calls, {} cache hits, {} retries, ~{}k out tokens",
        usage.calls, usage.cache_hits, usage.retries, usage.output_tokens / 1000);
    println!("===========================================");

    // write scorecard artifact
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let dir = args.out.unwrap_or_else(|| format!("runs/validate-{ts}"));
    let _ = std::fs::create_dir_all(&dir);
    let scorecard = serde_json::json!({
        "headline": headline, "gate": gate, "passed": passed, "n": n, "seed": seed,
        "categories": categories, "report": report, "usage": usage, "timestamp": ts,
    });
    let path = format!("{dir}/scorecard.json");
    if std::fs::write(&path, serde_json::to_string_pretty(&scorecard).unwrap()).is_ok() {
        println!("  scorecard -> {path}");
    }

    if passed { 0 } else { 1 }
}

fn market_poll(m: &MarketEntry) -> Poll {
    Poll {
        question: m.question.clone(),
        description: m.description.clone(),
        framing: Framing::Belief,
        as_of_date: m.as_of_date.clone(),
        model: Some(m.model.clone()),
        population: None,
        event: None,
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn pass_mark(b: bool) -> &'static str {
    if b {
        "PASS"
    } else {
        "FAIL"
    }
}
