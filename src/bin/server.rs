//! Axum HTTP server for the SF digital twin. Binds 0.0.0.0:$PORT (default 8080) so it
//! runs on fly.io. Loads `.env` for local dev; fly injects secrets as env vars.

use simfrancisco::api;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    simfrancisco::load_dotenv(".env");
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,simfrancisco=info".into()),
        )
        .init();

    let port: u16 = std::env::var("PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let tiles_path = std::env::var("TILES_DB").unwrap_or_else(|_| "tiles.db".to_string());
    let cache_path = std::env::var("CACHE_DB").unwrap_or_else(|_| "cache.db".to_string());
    let state_db = std::env::var("STATE_DB").unwrap_or_else(|_| "state.db".to_string());

    tracing::info!("loading state: tiles={tiles_path} cache={cache_path} state={state_db}");
    let state = api::build_state(&tiles_path, Some(&cache_path), &state_db)?;
    tracing::info!(
        "loaded {} SF PUMS records; map {}x{} chunks",
        state.records.len(),
        state.tiles.manifest.chunks_x,
        state.tiles.manifest.chunks_y
    );

    let app = api::router(state);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
