//! Axum HTTP API. Every documented endpoint (BRIEF §9/§10) with contract-testable
//! request/response shapes. The prediction engine is reachable without the life-sim;
//! the life-sim drives positions + the SSE stream for the frontend.

use crate::agent::Agent;
use crate::geo::TilesDb;
use crate::model::{Cache, Model, ModelClient};
use crate::persona::{build_population, Population};
use crate::predict::{Engine, Event, Framing, Poll};
use crate::pums::PumsRecord;
use crate::sim::{SimEngine, SimEvent};
use crate::state::{AgentState, SimState};
use crate::store::{SimMeta, Store};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::sse::{Event as SseEvent, Sse},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct AppState {
    pub client: ModelClient,
    pub engine: Engine,
    pub tiles: Arc<TilesDb>,
    pub records: Arc<Vec<PumsRecord>>,
    pub store: Arc<Store>,
    pub sims: Arc<Mutex<HashMap<String, Arc<SimContext>>>>,
    pub model_ok: Arc<Mutex<Option<bool>>>,
}

pub struct SimContext {
    pub id: String,
    pub meta: SimMeta,
    pub population: Arc<Population>,
    pub branches: Mutex<HashMap<String, Arc<BranchState>>>,
    pub counter: Mutex<u64>,
}

pub struct BranchState {
    pub id: String,
    pub sim_id: String,
    pub name: String,
    pub kind: String,
    pub engine: Mutex<SimEngine>,
    pub mode: String,
}

pub fn router(state: AppState) -> Router {
    use tower_http::cors::{Any, CorsLayer};
    let cors = CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any);
    Router::new()
        .route("/health", get(health))
        .route("/", get(root))
        .route("/simulations", post(create_sim))
        .route("/simulations/:id/demographics", get(demographics))
        .route("/simulations/:id/branches", post(create_branch))
        .route("/simulations/:id/reset-to-main", post(reset_to_main))
        .route("/branches/:bid", get(branch_status))
        .route("/branches/:bid", delete(delete_branch))
        .route("/branches/:bid/agents", get(branch_agents))
        .route("/branches/:bid/poll", post(branch_poll))
        .route("/branches/:bid/predict-market", post(predict_market))
        .route("/branches/:bid/stream", get(branch_stream))
        .layer(cors)
        .with_state(state)
}

async fn root() -> impl IntoResponse {
    Json(json!({
        "service": "sf-digital-twin",
        "docs": "see INTEGRATION.md",
        "endpoints": [
            "GET /health", "POST /simulations", "GET /simulations/{id}/demographics",
            "POST /simulations/{id}/branches", "GET /branches/{id}",
            "GET /branches/{id}/agents", "POST /branches/{id}/poll",
            "POST /branches/{id}/predict-market", "POST /simulations/{id}/reset-to-main",
            "DELETE /branches/{id}", "GET /branches/{id}/stream"
        ]
    }))
}

async fn health(State(st): State<AppState>) -> impl IntoResponse {
    // Liveness must never block. Model reachability is checked ONCE in the background and
    // memoized; until the first check returns, we report `null` (checking).
    let cached = *st.model_ok.lock().unwrap();
    if cached.is_none() && st.client.has_key() {
        let client = st.client.clone();
        let slot = st.model_ok.clone();
        tokio::spawn(async move {
            let ok = client.complete(Model::Grok43, "", "Reply with: OK", 16).await.is_ok();
            *slot.lock().unwrap() = Some(ok);
        });
    }
    Json(json!({
        "status": "ok",
        "model_reachable": cached,
        "has_key": st.client.has_key(),
        "map_chunks": st.tiles.manifest.chunks_x * st.tiles.manifest.chunks_y,
        "sf_pums_records": st.records.len(),
        "usage": st.client.usage.snapshot(),
    }))
}

#[derive(Deserialize)]
struct CreateSimReq {
    #[serde(default = "default_n")]
    n: usize,
    #[serde(default = "default_seed")]
    seed: u64,
    #[serde(default = "default_start")]
    start_datetime: String,
    #[serde(default = "default_tick")]
    tick_seconds: i64,
    #[serde(default = "default_commit")]
    commit_every: u64,
    #[serde(default)]
    distributional_params: Option<Value>,
}
fn default_n() -> usize { 800 }
fn default_seed() -> u64 { 42 }
fn default_start() -> String { "2024-11-01T08:00:00Z".to_string() }
fn default_tick() -> i64 { 30 }
fn default_commit() -> u64 { 20 }

async fn create_sim(State(st): State<AppState>, Json(req): Json<CreateSimReq>) -> impl IntoResponse {
    let n = req.n.clamp(1, 50_000);
    let pop = build_population(&st.records, n, req.seed, Some(&st.tiles));
    let sim_id = format!("sim-{}-{}-{}", req.seed, n, short_hash(&format!("{}{}", req.start_datetime, req.tick_seconds)));
    let meta = SimMeta {
        seed: req.seed,
        n,
        start_datetime: req.start_datetime.clone(),
        tick_seconds: req.tick_seconds,
        commit_every: req.commit_every,
    };
    let start_secs = parse_iso(&req.start_datetime);

    // main-branch engine
    let pop_arc = Arc::new(pop);
    let engine = SimEngine::new(st.tiles.clone(), pop_arc.clone(), start_secs, req.tick_seconds);

    // persist static layer + init snapshot for the branching store
    let static_blob = serde_json::to_string(&StaticLayer::from_pop(&pop_arc)).unwrap_or_default();
    let init_state = engine.state.clone();
    let _ = st.store.create_sim(&sim_id, &meta, &static_blob, &init_state);

    let main = Arc::new(BranchState {
        id: format!("{sim_id}:main"),
        sim_id: sim_id.clone(),
        name: "main".into(),
        kind: "main".into(),
        engine: Mutex::new(engine),
        mode: "clean".into(),
    });
    let ctx = Arc::new(SimContext {
        id: sim_id.clone(),
        meta,
        population: pop_arc,
        branches: Mutex::new(HashMap::from([(main.id.clone(), main)])),
        counter: Mutex::new(0),
    });
    st.sims.lock().unwrap().insert(sim_id.clone(), ctx);
    let _ = req.distributional_params; // accepted; reserved for per-demographic seeding

    (StatusCode::CREATED, Json(json!({
        "simulation_id": sim_id,
        "n": n,
        "main_branch": format!("{sim_id}:main"),
        "start_datetime": req.start_datetime,
    })))
}

async fn demographics(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let ctx = match st.sims.lock().unwrap().get(&id).cloned() {
        Some(c) => c,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error":"simulation not found"}))).into_response(),
    };
    // target marginals = full SF PUMS (weighted) == ACS; empirical = sampled population.
    let target = marginals_from_records(&st.records);
    let empirical = marginals_from_agents(&ctx.population.agents);
    let tol = 0.05;
    let mut comparison = serde_json::Map::new();
    let mut all_pass = true;
    for (var, emp) in &empirical {
        let tgt = target.get(var).cloned().unwrap_or_default();
        let dist = tv_dist(emp, &tgt);
        let pass = dist <= tol;
        all_pass &= pass;
        comparison.insert(var.clone(), json!({
            "empirical": emp, "target_acs": tgt, "tv_distance": dist, "pass": pass, "tolerance": tol
        }));
    }
    Json(json!({
        "simulation_id": id,
        "n_agents": ctx.population.agents.len(),
        "total_weight": ctx.population.total_weight(),
        "variables": comparison,
        "all_within_tolerance": all_pass,
    })).into_response()
}

#[derive(Deserialize)]
struct CreateBranchReq {
    #[serde(default)]
    event: Option<BranchEvent>,
    #[serde(default = "default_ticks")]
    ticks: usize,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    name: Option<String>,
}
#[derive(Deserialize)]
struct BranchEvent {
    text: String,
    #[serde(default)]
    progressive_coded: Option<bool>,
}
fn default_ticks() -> usize { 20 }

async fn create_branch(State(st): State<AppState>, Path(id): Path<String>, Json(req): Json<CreateBranchReq>) -> impl IntoResponse {
    let ctx = match st.sims.lock().unwrap().get(&id).cloned() {
        Some(c) => c,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error":"simulation not found"}))).into_response(),
    };
    let bnum = { let mut c = ctx.counter.lock().unwrap(); *c += 1; *c };
    let branch_id = format!("{id}:b{bnum}");
    let name = req.name.unwrap_or_else(|| format!("branch-{bnum}"));
    let ticks = req.ticks.min(2000);

    // clone main's current state into the new branch engine
    let main_state = {
        let m = ctx.branches.lock().unwrap();
        let main = m.get(&format!("{id}:main")).unwrap().clone();
        let e = main.engine.lock().unwrap();
        e.state.clone()
    };
    let mut engine = SimEngine::from_state(st.tiles.clone(), ctx.population.clone(), main_state, ctx.meta.tick_seconds);

    // apply the broadcast event (reactions + episodic memory) then run k ticks
    let mut reactions = 0usize;
    let mut event_text = String::new();
    if let Some(ev) = &req.event {
        let prog = ev.progressive_coded.unwrap_or(true);
        let evs = engine.broadcast(&branch_id, prog);
        reactions = evs.len();
        event_text = ev.text.clone();
        // record the event into agents' episodic memory (bounded) so social-mode polls see it
        let tag = format!("heard: {}", truncate_words(&ev.text, 16));
        for a in engine.state.agents.iter_mut() {
            a.memory.push(tag.clone());
            if a.memory.len() > 8 {
                a.memory.remove(0);
            }
        }
    }
    for _ in 0..ticks {
        let _ = engine.tick();
    }

    // persist branch snapshot in the store
    let main_head = st.store.branch_head(&format!("{id}:main")).ok();
    if let Some(head) = main_head {
        if let Ok(info) = st.store.create_branch(&id, head, &branch_id, &name) {
            let _ = st.store.commit(&id, &branch_id, &engine.state, "after-ticks");
            let _ = info;
        }
    }

    let clock = engine.state.clock_secs;
    let tick = engine.state.tick;
    let bs = Arc::new(BranchState {
        id: branch_id.clone(),
        sim_id: id.clone(),
        name: name.clone(),
        kind: "branch".into(),
        engine: Mutex::new(engine),
        mode: req.mode.clone().unwrap_or_else(|| "social".into()),
    });
    ctx.branches.lock().unwrap().insert(branch_id.clone(), bs);
    let _ = req.model;

    (StatusCode::CREATED, Json(json!({
        "branch_id": branch_id,
        "name": name,
        "ticks_run": ticks,
        "reactions_emitted": reactions,
        "event": event_text,
        "clock": crate::sim::secs_to_iso(clock),
        "tick": tick,
    }))).into_response()
}

async fn branch_status(State(st): State<AppState>, Path(bid): Path<String>) -> impl IntoResponse {
    match find_branch(&st, &bid) {
        Some((_ctx, bs)) => {
            let e = bs.engine.lock().unwrap();
            let alive = e.state.agents.iter().filter(|a| a.alive).count();
            Json(json!({
                "branch_id": bs.id,
                "sim_id": bs.sim_id,
                "name": bs.name,
                "kind": bs.kind,
                "mode": bs.mode,
                "status": "ready",
                "tick": e.state.tick,
                "clock": crate::sim::secs_to_iso(e.state.clock_secs),
                "agents_alive": alive,
            })).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({"error":"branch not found"}))).into_response(),
    }
}

async fn delete_branch(State(st): State<AppState>, Path(bid): Path<String>) -> impl IntoResponse {
    if bid.ends_with(":main") {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"cannot delete main"}))).into_response();
    }
    if let Some((ctx, _)) = find_branch(&st, &bid) {
        ctx.branches.lock().unwrap().remove(&bid);
        let _ = st.store.delete_branch(&bid);
        return Json(json!({"deleted": bid})).into_response();
    }
    (StatusCode::NOT_FOUND, Json(json!({"error":"branch not found"}))).into_response()
}

async fn reset_to_main(State(st): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let ctx = match st.sims.lock().unwrap().get(&id).cloned() {
        Some(c) => c,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error":"simulation not found"}))).into_response(),
    };
    // drop all non-main branches; main is the canonical HEAD
    let removed: Vec<String> = {
        let mut m = ctx.branches.lock().unwrap();
        let keys: Vec<String> = m.keys().filter(|k| !k.ends_with(":main")).cloned().collect();
        for k in &keys {
            m.remove(k);
            let _ = st.store.delete_branch(k);
        }
        keys
    };
    let main_tick = {
        let m = ctx.branches.lock().unwrap();
        let main = m.get(&format!("{id}:main")).unwrap().clone();
        let e = main.engine.lock().unwrap();
        e.state.tick
    };
    Json(json!({"reset_to": format!("{id}:main"), "dropped_branches": removed, "main_tick": main_tick})).into_response()
}

#[derive(Deserialize)]
struct AgentsQuery {
    #[serde(default)]
    filter: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}
fn default_limit() -> usize { 500 }

async fn branch_agents(State(st): State<AppState>, Path(bid): Path<String>, Query(q): Query<AgentsQuery>) -> impl IntoResponse {
    let (ctx, bs) = match find_branch(&st, &bid) {
        Some(x) => x,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error":"branch not found"}))).into_response(),
    };
    let filt = parse_filter(q.filter.as_deref());
    let e = bs.engine.lock().unwrap();
    let limit = q.limit.clamp(1, 5000);
    let mut out = Vec::new();
    let mut matched = 0usize;
    for ast in e.state.agents.iter() {
        let agent = match ctx.population.agents.get(ast.id as usize) {
            Some(a) => a,
            None => continue, // born agents have no static persona record
        };
        if !filter_matches(agent, &filt) {
            continue;
        }
        matched += 1;
        if matched <= q.offset {
            continue;
        }
        if out.len() >= limit {
            continue;
        }
        let (lon, lat) = st.tiles.cell_to_lonlat(ast.pos);
        out.push(json!({
            "id": ast.id,
            "name": agent.name,
            "action": ast.action,
            "alive": ast.alive,
            "cell": [ast.pos.x, ast.pos.y],
            "lonlat": [lon, lat],
            "neighborhood": agent.neighborhood,
            "age": agent.rec.age,
            "race_eth": agent.rec.race_eth(),
            "educ": agent.rec.educ(),
            "values": agent.values,
        }));
    }
    Json(json!({
        "branch_id": bid,
        "total_matched": matched,
        "offset": q.offset,
        "count": out.len(),
        "agents": out,
    })).into_response()
}

async fn branch_poll(State(st): State<AppState>, Path(bid): Path<String>, Json(req): Json<Value>) -> impl IntoResponse {
    let (ctx, _bs) = match find_branch(&st, &bid) {
        Some(x) => x,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error":"branch not found"}))).into_response(),
    };
    let poll = match poll_from_json(&req) {
        Ok(p) => p,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({"error": e}))).into_response(),
    };
    match st.engine.run_poll(&ctx.population, &poll).await {
        Ok(res) => Json(json!(res)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("poll failed: {e}")}))).into_response(),
    }
}

async fn predict_market(State(st): State<AppState>, Path(bid): Path<String>, Json(req): Json<Value>) -> impl IntoResponse {
    let (ctx, _bs) = match find_branch(&st, &bid) {
        Some(x) => x,
        None => return (StatusCode::NOT_FOUND, Json(json!({"error":"branch not found"}))).into_response(),
    };
    let question = req.get("question").and_then(|x| x.as_str()).unwrap_or("").to_string();
    if question.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"question required"}))).into_response();
    }
    let as_of = req.get("as_of_date").and_then(|x| x.as_str()).unwrap_or("2024-01-01").to_string();
    let bucket = req.get("bucket").and_then(|x| x.as_str()).unwrap_or("sf_opinion_informative").to_string();
    let poll = Poll {
        question: question.clone(),
        description: req.get("description").and_then(|x| x.as_str()).unwrap_or("Prediction-market question mapped to a pollable belief.").to_string(),
        framing: Framing::Belief,
        as_of_date: as_of.clone(),
        model: req.get("model").and_then(|x| x.as_str()).map(|s| s.to_string()),
        population: None,
        event: None,
    };
    match st.engine.run_poll(&ctx.population, &poll).await {
        Ok(res) => Json(json!({
            "question": question,
            "as_of_date": as_of,
            "bucket": bucket,
            "sim_probability_yes": res.p_yes,
            "ci": [res.ci_low, res.ci_high],
            "n_agents": res.n_agents,
            "model": res.model,
            "note": "headline market number weights the sf_opinion_informative bucket; general_knowledge is reported separately.",
            "live_market_price": req.get("live_market_price"),
        })).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, Json(json!({"error": format!("market poll failed: {e}")}))).into_response(),
    }
}

async fn branch_stream(State(st): State<AppState>, Path(bid): Path<String>) -> impl IntoResponse {
    let bs = match find_branch(&st, &bid) {
        Some((_, bs)) => bs,
        None => {
            let s = async_stream::stream! {
                yield Ok::<_, Infallible>(SseEvent::default().event("error").data("{\"error\":\"branch not found\"}"));
            };
            return Sse::new(Box::pin(s) as std::pin::Pin<Box<dyn futures::Stream<Item = Result<SseEvent, Infallible>> + Send>>).into_response();
        }
    };
    let stream = async_stream::stream! {
        // initial snapshot so a client sees a typed event immediately.
        // Scope the MutexGuard so it never lives across a yield/await (guards are !Send).
        let snap = {
            let e = bs.engine.lock().unwrap();
            json!({
                "type": "snapshot",
                "tick": e.state.tick,
                "clock": crate::sim::secs_to_iso(e.state.clock_secs),
                "agents_alive": e.state.agents.iter().filter(|a| a.alive).count(),
            })
        };
        yield Ok::<_, Infallible>(SseEvent::default().event("snapshot").data(snap.to_string()));

        // live ticks (bounded so a contract test terminates)
        for _ in 0..600u32 {
            let events: Vec<SimEvent> = { bs.engine.lock().unwrap().tick() };
            for ev in events {
                let data = serde_json::to_string(&ev).unwrap_or_default();
                let name = sse_event_name(&ev);
                yield Ok::<_, Infallible>(SseEvent::default().event(name).data(data));
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    };
    Sse::new(Box::pin(stream) as std::pin::Pin<Box<dyn futures::Stream<Item = Result<SseEvent, Infallible>> + Send>>)
        .keep_alive(axum::response::sse::KeepAlive::default())
        .into_response()
}

fn sse_event_name(ev: &SimEvent) -> &'static str {
    match ev {
        SimEvent::AgentMoved { .. } => "agent_moved",
        SimEvent::AgentSaid { .. } => "agent_said",
        SimEvent::AgentReacted { .. } => "agent_reacted",
        SimEvent::Tick { .. } => "tick",
        SimEvent::Birth { .. } => "birth",
        SimEvent::Death { .. } => "death",
    }
}

// ---- helpers ----

fn find_branch(st: &AppState, bid: &str) -> Option<(Arc<SimContext>, Arc<BranchState>)> {
    let sim_id = bid.split(':').next().map(|s| {
        // sim ids contain ':'? No — sim id is the part before the last ':bN' / ':main'.
        s.to_string()
    });
    let _ = sim_id;
    // sim_id is everything up to the last ':'
    let sim_id = match bid.rfind(':') {
        Some(i) => bid[..i].to_string(),
        None => return None,
    };
    let ctx = st.sims.lock().unwrap().get(&sim_id).cloned()?;
    let bs = ctx.branches.lock().unwrap().get(bid).cloned()?;
    Some((ctx, bs))
}

fn poll_from_json(req: &Value) -> Result<Poll, String> {
    let question = req.get("question").and_then(|x| x.as_str()).ok_or("question required")?.to_string();
    let description = req.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let as_of_date = req.get("as_of_date").and_then(|x| x.as_str()).unwrap_or("2024-01-01").to_string();
    let framing = match req.get("framing").and_then(|x| x.as_str()) {
        Some("belief") => Framing::Belief,
        _ => Framing::Vote,
    };
    let event = req.get("event").and_then(|e| e.as_str()).map(|t| Event { text: t.to_string(), as_of_date: as_of_date.clone() });
    Ok(Poll {
        question,
        description,
        framing,
        as_of_date,
        model: req.get("model").and_then(|x| x.as_str()).map(|s| s.to_string()),
        population: req.get("population").and_then(|x| x.as_str()).map(|s| s.to_string()),
        event,
    })
}

#[derive(Default)]
struct Filter {
    pairs: Vec<(String, String)>,
}
fn parse_filter(s: Option<&str>) -> Filter {
    let mut f = Filter::default();
    if let Some(s) = s {
        for part in s.split([',', '&']) {
            if let Some((k, v)) = part.split_once(['=', ':']) {
                f.pairs.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
    }
    f
}
fn filter_matches(a: &Agent, f: &Filter) -> bool {
    for (k, v) in &f.pairs {
        let ok = match k.as_str() {
            "race" | "race_eth" => a.rec.race_eth() == v,
            "educ" => a.rec.educ() == v,
            "age_band" => a.rec.age_band() == v,
            "puma" => a.rec.puma.to_string() == *v,
            "tenure" => (if a.homeowner { "own" } else { "rent" }) == v,
            "sex" => a.rec.sex.to_string() == *v,
            "religion" => a.religion.label().contains(v.as_str()),
            _ => true,
        };
        if !ok {
            return false;
        }
    }
    true
}

fn marginals_from_records(recs: &[PumsRecord]) -> HashMap<String, HashMap<String, f64>> {
    let mut m: HashMap<String, HashMap<String, f64>> = HashMap::new();
    for r in recs {
        add_marginal(&mut m, "age_band", r.age_band(), r.pwgtp);
        add_marginal(&mut m, "race_eth", r.race_eth(), r.pwgtp);
        add_marginal(&mut m, "educ", r.educ(), r.pwgtp);
        add_marginal(&mut m, "sex", if r.sex == 1 { "male" } else { "female" }, r.pwgtp);
        add_marginal(&mut m, "citizen", if r.is_citizen() { "yes" } else { "no" }, r.pwgtp);
    }
    normalize(&mut m);
    m
}
fn marginals_from_agents(agents: &[Agent]) -> HashMap<String, HashMap<String, f64>> {
    let mut m: HashMap<String, HashMap<String, f64>> = HashMap::new();
    for a in agents {
        let w = a.weight();
        add_marginal(&mut m, "age_band", a.rec.age_band(), w);
        add_marginal(&mut m, "race_eth", a.rec.race_eth(), w);
        add_marginal(&mut m, "educ", a.rec.educ(), w);
        add_marginal(&mut m, "sex", if a.rec.sex == 1 { "male" } else { "female" }, w);
        add_marginal(&mut m, "citizen", if a.rec.is_citizen() { "yes" } else { "no" }, w);
    }
    normalize(&mut m);
    m
}
fn add_marginal(m: &mut HashMap<String, HashMap<String, f64>>, var: &str, level: &str, w: f64) {
    *m.entry(var.to_string()).or_default().entry(level.to_string()).or_insert(0.0) += w;
}
fn normalize(m: &mut HashMap<String, HashMap<String, f64>>) {
    for (_, dist) in m.iter_mut() {
        let total: f64 = dist.values().sum();
        if total > 0.0 {
            for v in dist.values_mut() {
                *v /= total;
            }
        }
    }
}
fn tv_dist(a: &HashMap<String, f64>, b: &HashMap<String, f64>) -> f64 {
    let mut keys: std::collections::HashSet<&String> = a.keys().collect();
    keys.extend(b.keys());
    let mut d = 0.0;
    for k in keys {
        d += (a.get(k).copied().unwrap_or(0.0) - b.get(k).copied().unwrap_or(0.0)).abs();
    }
    d / 2.0
}

#[derive(serde::Serialize)]
struct StaticLayer {
    n: usize,
    seed: u64,
}
impl StaticLayer {
    fn from_pop(p: &Population) -> Self {
        StaticLayer { n: p.agents.len(), seed: p.seed }
    }
}

fn parse_iso(s: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp())
        .unwrap_or(1730448000) // 2024-11-01T08:00:00Z fallback
}

fn truncate_words(s: &str, n: usize) -> String {
    s.split_whitespace().take(n).collect::<Vec<_>>().join(" ")
}

fn short_hash(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(&h.finalize()[..4])
}

/// Build the full AppState from environment (loads PUMS + tiles + opens caches).
pub fn build_state(tiles_path: &str, cache_path: Option<&str>, state_db: &str) -> anyhow::Result<AppState> {
    let cache = match cache_path {
        Some(p) => Some(Arc::new(Cache::open(p)?)),
        None => None,
    };
    let client = ModelClient::from_env(cache)?;
    let engine = Engine::new(client.clone());
    let tiles = Arc::new(TilesDb::open(tiles_path)?);
    let records = Arc::new(crate::pums::load_sf()?);
    let store = Arc::new(Store::open(state_db)?);
    Ok(AppState {
        client,
        engine,
        tiles,
        records,
        store,
        sims: Arc::new(Mutex::new(HashMap::new())),
        model_ok: Arc::new(Mutex::new(None)),
    })
}

// silence unused imports used only in some build configs
#[allow(unused_imports)]
use crate::state as _state;
#[allow(dead_code)]
fn _touch(_: AgentState, _: SimState) {}
