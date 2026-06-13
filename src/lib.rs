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
pub mod geo;
pub mod model;
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

/// Load `.env` into the process environment (only vars not already set). No external
/// crate; tolerant of `KEY=VALUE` and comment/blank lines. fly.io injects secrets as
/// real env vars, so this is a no-op there.
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
                if std::env::var(k).is_err() {
                    std::env::set_var(k, v);
                }
            }
        }
    }
}
