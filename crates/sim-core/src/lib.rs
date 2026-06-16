//! SF Digital Twin backend library.
//!
//! Two engines over a shared persona layer:
//!   - `predict`  : the scored prediction engine (persona + as-of-date + event -> weighted opinion)
//!   - `sim`      : the bounded-cost life simulation (movement, schedules, conversations)
//!
//! See BRIEF.md for the full architecture. The prediction engine is a hard
//! dependency of the demo's impact and runs without the life-sim.

pub mod aggregate;
pub mod agent;
pub mod city;
pub mod geo;
pub mod lifestyle;
pub mod model;
pub mod news;
pub mod parse;
pub mod pathfind;
pub mod persona;
pub mod predict;
pub mod pums;
pub mod religion;
pub mod rubric;
pub mod sim;
pub mod state;
pub mod store;

pub mod api;

/// Re-exports for binaries and tests.
pub use model::{Cache, Model, ModelClient};

/// Load `.env` into the process environment. The `.env` file is the project's config
/// source of truth, so a present `.env` value OVERRIDES any inherited shell env var
/// (otherwise a stray system-wide `ANTHROPIC_API_KEY`/`MODEL_API_KEY` would silently
/// shadow the project key). fly.io has no `.env` file in the image, so this is a no-op
/// there and the fly secrets are used. No external crate; tolerant of comments/blanks.
pub fn load_dotenv(path: &str) {
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                let k = k.trim();
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    std::env::set_var(k, v); // .env wins over inherited shell env
                }
            }
        }
    }
}
