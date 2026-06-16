//! daemon — the "constantly running" hook. Advances each city's clock (served
//! knowledge date) and refreshes its news cache from real news search.
//!
//! Run on a schedule (cron / fly scheduled machine):
//!     cargo run --bin daemon -- [YYYY-MM-DD]
//! With NEWS_API_KEY set it pulls fresh headlines (newsapi.org); otherwise it bumps
//! the date and keeps the existing cache (which the seed step populated). The served
//! knowledge cutoff = this date, and live polls inject these headlines as context.

use simfrancisco::news::{self, Article, CityNews};

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
        .unwrap_or_else(today);
    let api_key = std::env::var("NEWS_API_KEY").ok();

    for (slug, query) in CITIES {
        let mut cn = news::load(slug);
        cn.city = slug.to_string();
        cn.date = date.clone();
        match &api_key {
            Some(key) => match fetch_newsapi(query, key, &date).await {
                Ok(arts) if !arts.is_empty() => {
                    eprintln!("[daemon] {slug}: refreshed {} articles", arts.len());
                    cn.articles = arts;
                }
                _ => eprintln!("[daemon] {slug}: news fetch failed; kept cache, bumped date"),
            },
            None => eprintln!("[daemon] {slug}: no NEWS_API_KEY; bumped clock to {date} (cache kept)"),
        }
        news::save(&cn)?;
    }
    eprintln!("[daemon] all cities advanced to {date}");
    Ok(())
}

/// Pull recent headlines for a city from newsapi.org and map to our Article shape.
async fn fetch_newsapi(query: &str, api_key: &str, date: &str) -> anyhow::Result<Vec<Article>> {
    let url = format!(
        "https://newsapi.org/v2/everything?q={}&language=en&sortBy=publishedAt&pageSize=6&apiKey={}",
        urlencoding(query),
        api_key
    );
    let client = reqwest::Client::builder()
        .user_agent("sim-francisco-daemon")
        .build()?;
    let v: serde_json::Value = client.get(&url).send().await?.json().await?;
    let mut out = Vec::new();
    if let Some(arr) = v.get("articles").and_then(|x| x.as_array()) {
        for a in arr.iter().take(6) {
            out.push(Article {
                headline: a.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                summary: a.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                topic: "news".to_string(),
                salience: String::new(),
                url: a.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                date: date.to_string(),
            });
        }
    }
    Ok(out)
}

fn urlencoding(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_string() } else { format!("%{:02X}", c as u32) })
        .collect()
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

// keep CityNews import used even if the compiler can't see the trait path
#[allow(dead_code)]
fn _touch(_: CityNews) {}
