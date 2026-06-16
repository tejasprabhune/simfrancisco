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

/// Today's date (UTC) as YYYY-MM-DD — the served knowledge cutoff.
pub fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn urlencode(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_string() } else { format!("%{:02X}", c as u32) })
        .collect()
}

/// Pull recent headlines for a city from newsapi.org and map to our Article shape.
/// Best-effort; article dates use the real publish date when present.
pub async fn fetch_newsapi(query: &str, api_key: &str, date: &str) -> anyhow::Result<Vec<Article>> {
    // searchIn=title keeps headlines actually ABOUT the city (not articles that merely
    // mention it); sorted newest-first.
    let url = format!(
        "https://newsapi.org/v2/everything?q=%22{}%22&searchIn=title&language=en&sortBy=publishedAt&pageSize=10&apiKey={}",
        urlencode(query),
        api_key
    );
    let client = reqwest::Client::builder().user_agent("sim-francisco-daemon").build()?;
    let v: serde_json::Value = client.get(&url).send().await?.json().await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("articles").and_then(|x| x.as_array()) {
        for a in arr {
            let headline = a.get("title").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if headline.is_empty() || headline == "[Removed]" {
                continue;
            }
            let pub_date = a
                .get("publishedAt")
                .and_then(|x| x.as_str())
                .map(|s| s.chars().take(10).collect::<String>())
                .filter(|s| s.len() == 10)
                .unwrap_or_else(|| date.to_string());
            out.push(Article {
                headline,
                summary: a.get("description").and_then(|x| x.as_str()).unwrap_or("").trim().to_string(),
                topic: "news".to_string(),
                salience: String::new(),
                url: a.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                date: pub_date,
            });
            if out.len() >= 6 {
                break;
            }
        }
    }
    Ok(out)
}

/// Refresh every city's news cache to `date`. When NEWS_API_KEY is set, pull fresh
/// NewsAPI headlines per city; otherwise just advance the served clock and keep the
/// existing cache. Writes each city's cache file. Best-effort per city.
pub async fn refresh_all(cities: &[(String, String)], date: &str) {
    let api_key = std::env::var("NEWS_API_KEY").ok().filter(|k| !k.is_empty());
    for (slug, query) in cities {
        let mut cn = load(slug);
        cn.city = slug.clone();
        cn.date = date.to_string();
        if let Some(key) = &api_key {
            match fetch_newsapi(query, key, date).await {
                Ok(arts) if !arts.is_empty() => cn.articles = arts,
                _ => {} // fetch failed/empty: keep the cache, clock still advances
            }
        }
        if let Err(e) = save(&cn) {
            eprintln!("[news] {slug}: save failed: {e}");
        }
    }
}
