//! daemon — the "constantly running" hook for an external schedule. Advances each
//! city's clock (served knowledge date) and refreshes its news cache.
//!
//! The deployed server already does this in-process (see the NEWS_REFRESH_HOURS
//! task in bin/server.rs); this binary is the standalone form for cron / a fly
//! scheduled machine:
//!     cargo run --bin daemon -- [YYYY-MM-DD]
//! With NEWS_API_KEY set it pulls fresh headlines (newsapi.org); otherwise it bumps
//! the date and keeps the existing cache.

use simfrancisco::news;

const CITIES: [(&str, &str); 5] = [
    ("sf", "San Francisco"),
    ("neu_york", "New York City"),
    ("synth_la", "Los Angeles"),
    ("cybercago", "Chicago"),
    ("simami", "Miami"),
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    simfrancisco::load_dotenv(".env");
    let date = std::env::args()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .unwrap_or_else(news::today);
    let cities: Vec<(String, String)> =
        CITIES.iter().map(|(s, q)| (s.to_string(), q.to_string())).collect();
    news::refresh_all(&cities, &date).await;
    eprintln!("[daemon] all cities advanced to {date}");
    Ok(())
}
