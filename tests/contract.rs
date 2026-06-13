//! Endpoint contract tests. Runs two ways:
//!   - default `cargo test`: spins up the server IN-PROCESS in offline model mode and
//!     asserts the request/response contract of every endpoint (no LLM calls, no network).
//!   - against a LIVE url: set `CONTRACT_BASE_URL=https://<app>.fly.dev` to run the same
//!     shape tests plus a real poll/market against the deployed service.
//!
//! The same assertions back the deployment gate: every documented endpoint responds per
//! contract against the live URL.

use serde_json::Value;
use std::time::Duration;

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(120)).build().unwrap()
}

/// Returns (base_url, live?). In local mode, starts an in-process offline server.
async fn base() -> (String, bool) {
    if let Ok(url) = std::env::var("CONTRACT_BASE_URL") {
        return (url.trim_end_matches('/').to_string(), true);
    }
    // local in-process server, offline model mode (no token spend)
    std::env::set_var("MODEL_OFFLINE", "1");
    simfrancisco::load_dotenv(".env");
    let dir = std::env::temp_dir().join(format!("sf_contract_{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();
    let cache_path = dir.join("cache.db");
    let state_path = dir.join("state.db");
    let state = simfrancisco::api::build_state(
        "tiles.db",
        Some(cache_path.to_str().unwrap()),
        state_path.to_str().unwrap(),
    )
    .expect("build_state (need tiles.db + data/sf_pums.csv present)");
    let app = simfrancisco::api::router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    // give it a moment to start
    tokio::time::sleep(Duration::from_millis(150)).await;
    (format!("http://{addr}"), false)
}

#[tokio::test]
async fn contract_all_endpoints() {
    let (base, live) = base().await;
    let c = client();

    // ---- /health ----
    let r = c.get(format!("{base}/health")).send().await.expect("health");
    assert_eq!(r.status(), 200, "health status");
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["status"], "ok");
    assert!(v.get("has_key").is_some());
    assert!(v.get("sf_pums_records").and_then(|x| x.as_u64()).unwrap_or(0) > 1000, "pums loaded");

    // ---- POST /simulations ----
    let r = c
        .post(format!("{base}/simulations"))
        .json(&serde_json::json!({"n": 1500, "seed": 42, "start_datetime": "2024-11-01T08:00:00Z", "tick_seconds": 30, "commit_every": 20}))
        .send().await.expect("create sim");
    assert_eq!(r.status(), 201, "create sim status");
    let v: Value = r.json().await.unwrap();
    let sim_id = v["simulation_id"].as_str().expect("simulation_id").to_string();
    assert!(v["main_branch"].as_str().unwrap().ends_with(":main"));

    // ---- GET /simulations/{id}/demographics (marginals match ACS) ----
    let r = c.get(format!("{base}/simulations/{sim_id}/demographics")).send().await.unwrap();
    assert_eq!(r.status(), 200, "demographics status");
    let v: Value = r.json().await.unwrap();
    assert!(v["variables"].get("age_band").is_some(), "has age_band marginal");
    assert!(v["variables"].get("race_eth").is_some());
    assert_eq!(v["all_within_tolerance"], true, "sampled marginals must match ACS within tolerance: {}", v["variables"]);

    // ---- POST /simulations/{id}/branches (with event, runs ticks) ----
    let r = c
        .post(format!("{base}/simulations/{sim_id}/branches"))
        .json(&serde_json::json!({"event": {"text": "A major new transit measure is proposed.", "progressive_coded": true}, "ticks": 10, "mode": "social"}))
        .send().await.unwrap();
    assert_eq!(r.status(), 201, "create branch status");
    let v: Value = r.json().await.unwrap();
    let branch_id = v["branch_id"].as_str().expect("branch_id").to_string();
    assert!(v["ticks_run"].as_u64().unwrap() == 10);
    assert!(v["reactions_emitted"].as_u64().unwrap() > 0, "event produced reactions");

    // ---- GET /branches/{id} ----
    let r = c.get(format!("{base}/branches/{branch_id}")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["branch_id"], branch_id);
    assert!(v["tick"].as_u64().unwrap() >= 10);
    assert!(v.get("clock").and_then(|x| x.as_str()).is_some());

    // ---- GET /branches/{id}/agents?filter=...&limit ----
    let r = c.get(format!("{base}/branches/{branch_id}/agents?filter=tenure=rent&limit=25")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    let agents = v["agents"].as_array().expect("agents array");
    assert!(!agents.is_empty(), "agents returned");
    let a0 = &agents[0];
    for k in ["id", "name", "action", "cell", "lonlat", "neighborhood", "age", "values"] {
        assert!(a0.get(k).is_some(), "agent missing field {k}");
    }
    let cell = a0["cell"].as_array().unwrap();
    assert_eq!(cell.len(), 2, "cell is [x,y]");
    let lonlat = a0["lonlat"].as_array().unwrap();
    let lon = lonlat[0].as_f64().unwrap();
    let lat = lonlat[1].as_f64().unwrap();
    assert!((-122.55..-122.33).contains(&lon), "lon in SF bbox: {lon}");
    assert!((37.69..37.84).contains(&lat), "lat in SF bbox: {lat}");

    // ---- GET /branches/{id}/stream (SSE: first typed event) ----
    let r = c.get(format!("{base}/branches/{branch_id}/stream")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let ct = r.headers().get("content-type").and_then(|h| h.to_str().ok()).unwrap_or("");
    assert!(ct.contains("text/event-stream"), "SSE content-type, got {ct}");
    // read a small prefix of the stream and confirm a typed event arrives
    let body = read_sse_prefix(r, Duration::from_secs(8)).await;
    assert!(body.contains("event:"), "stream has SSE event lines");
    assert!(
        body.contains("snapshot") || body.contains("tick") || body.contains("agent_moved"),
        "stream emits a typed event; got: {}",
        &body[..body.len().min(300)]
    );

    // ---- POST /branches/{id}/poll  (validation contract; full path only with a model) ----
    let r = c.post(format!("{base}/branches/{branch_id}/poll")).json(&serde_json::json!({})).send().await.unwrap();
    assert_eq!(r.status(), 400, "poll without question must 400");

    if live {
        // real weighted poll against the deployed model
        let r = c
            .post(format!("{base}/branches/{branch_id}/poll"))
            .json(&serde_json::json!({
                "question": "Do you support more public transit funding in San Francisco?",
                "description": "A measure to expand bus and rail service funded by a modest tax.",
                "framing": "vote", "as_of_date": "2024-06-01", "model": "gpt-4o"
            }))
            .send().await.unwrap();
        assert_eq!(r.status(), 200, "live poll status");
        let v: Value = r.json().await.unwrap();
        let p = v["p_yes"].as_f64().expect("p_yes");
        assert!((0.0..=1.0).contains(&p), "p_yes in [0,1]: {p}");
        assert!(v["breakdowns"].get("age").is_some(), "has age breakdown");
        assert!(v["ci_low"].as_f64().unwrap() <= p && p <= v["ci_high"].as_f64().unwrap(), "CI brackets p_yes");

        // predict-market
        let r = c
            .post(format!("{base}/branches/{branch_id}/predict-market"))
            .json(&serde_json::json!({"question": "Will a Democrat win the 2024 California U.S. Senate seat?", "as_of_date": "2024-06-01", "bucket": "sf_opinion_informative", "model": "gpt-4o"}))
            .send().await.unwrap();
        assert_eq!(r.status(), 200, "predict-market status");
        let v: Value = r.json().await.unwrap();
        assert!(v["sim_probability_yes"].as_f64().is_some());
        assert_eq!(v["bucket"], "sf_opinion_informative");
    }

    // ---- POST /simulations/{id}/reset-to-main ----
    let r = c.post(format!("{base}/simulations/{sim_id}/reset-to-main")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert!(v["reset_to"].as_str().unwrap().ends_with(":main"));

    // ---- DELETE /branches/{id} (already dropped by reset; should 404 or ok) ----
    let r = c.delete(format!("{base}/branches/{branch_id}")).send().await.unwrap();
    assert!(r.status() == 200 || r.status() == 404, "delete branch status {}", r.status());

    // ---- 404 contract ----
    let r = c.get(format!("{base}/branches/does-not-exist:b9")).send().await.unwrap();
    assert_eq!(r.status(), 404, "unknown branch 404");
}

async fn read_sse_prefix(resp: reqwest::Response, timeout: Duration) -> String {
    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = String::new();
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                buf.push_str(&String::from_utf8_lossy(&chunk));
                if buf.contains("event:") && buf.len() > 40 {
                    break;
                }
            }
            _ => break,
        }
    }
    buf
}
