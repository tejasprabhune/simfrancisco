//! News layer. A per-city recent-news cache at `data/news/<slug>.json`, seeded by the
//! `daemon` binary from real web/news search. Two uses:
//!   1. the frontend news bubble (`GET /cities/{city}/news`),
//!   2. injecting today's events into LIVE polls so predictions reflect current
//!      knowledge (the served "knowledge cutoff = today's news"). Backtests with an
//!      old as_of_date never see this, so the leakage-free historical tests are intact.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CityNews {
    pub city: String,
    pub date: String,
    pub articles: Vec<Article>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Article {
    pub headline: String,
    pub summary: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub salience: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub date: String,
}

pub fn path(slug: &str) -> String {
    format!("data/news/{slug}.json")
}

pub fn load(slug: &str) -> CityNews {
    match std::fs::read_to_string(path(slug)) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
        Err(_) => CityNews::default(),
    }
}

/// The N most recent articles (the cache is stored newest-first by convention).
pub fn recent(slug: &str, n: usize) -> Vec<Article> {
    let mut a = load(slug).articles;
    a.truncate(n);
    a
}

/// A neutral prompt block of the city's recent news for injection into live polls.
/// Empty when there is no news. Residents "are aware of" these headlines.
pub fn prompt_block(slug: &str) -> String {
    let news = load(slug);
    if news.articles.is_empty() {
        return String::new();
    }
    let mut s = format!(
        "Recent local and national news that residents are aware of (as of {}):\n",
        news.date
    );
    for a in news.articles.iter().take(6) {
        s.push_str(&format!("- {}. {}\n", a.headline, a.summary));
    }
    s
}

pub fn save(news: &CityNews) -> anyhow::Result<()> {
    std::fs::create_dir_all("data/news")?;
    std::fs::write(path(&news.city), serde_json::to_string_pretty(news)?)?;
    Ok(())
}
