//! Azure AI Foundry model client (raw HTTP, two request shapes).
//!
//! Three deployments share one base URL `…/openai/v1`:
//!   - grok-4.3  -> POST /chat/completions  body {model, messages, max_tokens}
//!                 read choices[0].message.content
//!   - gpt-4o    -> POST /responses         body {model, input, max_output_tokens}
//!   - gpt-5.5   -> POST /responses          (reasoning model)
//!                 read the output[] element whose type == "message", .content[0].text
//!
//! Auth is `Authorization: Bearer $MODEL_API_KEY` (verified live; falls back to
//! the `api-key:` header on a 401). Concurrency is bounded by a semaphore; 429/5xx
//! get exponential backoff + jitter. A sqlite cache makes "clean mode" reproducible
//! and free on re-run (cache key = sha256(model|system|user|max_tokens)).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Model {
    Gpt4o,
    Gpt55,
    Grok43,
}

impl Model {
    pub fn id(&self) -> &'static str {
        match self {
            Model::Gpt4o => "gpt-4o",
            Model::Gpt55 => "gpt-5.5",
            Model::Grok43 => "grok-4.3",
        }
    }
    /// gpt-4o / gpt-5.5 use the /responses shape; grok-4.3 uses /chat/completions.
    pub fn uses_responses(&self) -> bool {
        matches!(self, Model::Gpt4o | Model::Gpt55)
    }
    pub fn parse(s: &str) -> Model {
        match s.trim().to_ascii_lowercase().as_str() {
            "gpt-4o" | "gpt4o" | "4o" => Model::Gpt4o,
            "gpt-5.5" | "gpt55" | "gpt-55" | "5.5" => Model::Gpt55,
            "grok-4.3" | "grok" | "grok43" => Model::Grok43,
            _ => Model::Gpt4o,
        }
    }
}

impl std::fmt::Display for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// Sqlite-backed response cache. Makes clean-mode validation deterministic across
/// re-runs and avoids paying twice for the same (model, prompt).
pub struct Cache {
    conn: Mutex<rusqlite::Connection>,
}

impl Cache {
    pub fn open(path: &str) -> Result<Self> {
        let conn = rusqlite::Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS llm_cache (
                key TEXT PRIMARY KEY,
                model TEXT NOT NULL,
                response TEXT NOT NULL,
                created INTEGER NOT NULL
             );",
        )?;
        Ok(Cache { conn: Mutex::new(conn) })
    }
    fn get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT response FROM llm_cache WHERE key=?1",
            [key],
            |r| r.get::<_, String>(0),
        )
        .ok()
    }
    fn put(&self, key: &str, model: &str, response: &str) {
        let conn = self.conn.lock().unwrap();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO llm_cache (key, model, response, created) VALUES (?1,?2,?3,strftime('%s','now'))",
            rusqlite::params![key, model, response],
        );
    }
}

#[derive(Default)]
pub struct Usage {
    pub calls: AtomicU64,
    pub cache_hits: AtomicU64,
    pub input_tokens: AtomicU64,
    pub output_tokens: AtomicU64,
    pub retries: AtomicU64,
}

impl Usage {
    pub fn snapshot(&self) -> UsageSnapshot {
        UsageSnapshot {
            calls: self.calls.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            input_tokens: self.input_tokens.load(Ordering::Relaxed),
            output_tokens: self.output_tokens.load(Ordering::Relaxed),
            retries: self.retries.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageSnapshot {
    pub calls: u64,
    pub cache_hits: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub retries: u64,
}

#[derive(Clone)]
pub struct ModelClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    sem: Arc<Semaphore>,
    max_retries: u32,
    cache: Option<Arc<Cache>>,
    pub usage: Arc<Usage>,
    /// when true, network is disabled and only cache hits succeed (offline/deterministic).
    offline: bool,
}

const DEFAULT_BASE: &str = "https://claude-day-resource.services.ai.azure.com/openai/v1";

impl ModelClient {
    /// Build from environment. `MODEL_API_KEY` is required for live calls.
    /// Base URL is derived from `OPENAI_API_URL` (stripping `/responses`) or the default.
    pub fn from_env(cache: Option<Arc<Cache>>) -> Result<Self> {
        let api_key = std::env::var("MODEL_API_KEY").unwrap_or_default();
        let base = std::env::var("OPENAI_API_URL")
            .ok()
            .map(|u| u.replace("/responses", "").replace("/chat/completions", ""))
            .filter(|u| u.contains("/openai/v1"))
            .unwrap_or_else(|| DEFAULT_BASE.to_string());
        let max_inflight: usize = std::env::var("MODEL_MAX_INFLIGHT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8);
        let offline = std::env::var("MODEL_OFFLINE").map(|v| v == "1").unwrap_or(false);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .connect_timeout(Duration::from_secs(15))
            .build()?;
        Ok(ModelClient {
            http,
            base,
            api_key,
            sem: Arc::new(Semaphore::new(max_inflight)),
            max_retries: 5,
            cache,
            usage: Arc::new(Usage::default()),
            offline,
        })
    }

    pub fn has_key(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn cache_key(model: Model, system: &str, user: &str, max_tokens: u32) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(model.id().as_bytes());
        h.update(b"\x00");
        h.update(system.as_bytes());
        h.update(b"\x00");
        h.update(user.as_bytes());
        h.update(b"\x00");
        h.update(max_tokens.to_le_bytes());
        hex::encode(h.finalize())
    }

    /// One completion. Returns the model's text. Uses cache first; on miss, calls the
    /// live endpoint with retry/backoff and stores the result.
    pub async fn complete(
        &self,
        model: Model,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let key = Self::cache_key(model, system, user, max_tokens);
        if let Some(c) = &self.cache {
            if let Some(hit) = c.get(&key) {
                self.usage.cache_hits.fetch_add(1, Ordering::Relaxed);
                return Ok(hit);
            }
        }
        if self.offline {
            return Err(anyhow!("offline mode: cache miss for {}", model.id()));
        }
        if self.api_key.is_empty() {
            return Err(anyhow!("MODEL_API_KEY not set"));
        }
        let text = self.call_live(model, system, user, max_tokens).await?;
        if let Some(c) = &self.cache {
            c.put(&key, model.id(), &text);
        }
        Ok(text)
    }

    async fn call_live(
        &self,
        model: Model,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<String> {
        let _permit = self.sem.acquire().await.unwrap();
        let (url, body) = if model.uses_responses() {
            let input = if system.is_empty() {
                user.to_string()
            } else {
                format!("{system}\n\n{user}")
            };
            (
                format!("{}/responses", self.base),
                json!({ "model": model.id(), "input": input, "max_output_tokens": max_tokens.max(16) }),
            )
        } else {
            let mut messages = Vec::new();
            if !system.is_empty() {
                messages.push(json!({"role":"system","content":system}));
            }
            messages.push(json!({"role":"user","content":user}));
            (
                format!("{}/chat/completions", self.base),
                json!({ "model": model.id(), "messages": messages, "max_tokens": max_tokens.max(16) }),
            )
        };

        let mut attempt = 0u32;
        loop {
            self.usage.calls.fetch_add(1, Ordering::Relaxed);
            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await;

            match resp {
                Ok(r) => {
                    let status = r.status();
                    if status.as_u16() == 401 {
                        // Fall back to api-key header path once.
                        let r2 = self
                            .http
                            .post(&url)
                            .header("api-key", &self.api_key)
                            .header("Content-Type", "application/json")
                            .json(&body)
                            .send()
                            .await?;
                        let s2 = r2.status();
                        let txt = r2.text().await.unwrap_or_default();
                        if s2.is_success() {
                            return self.extract(model, &txt);
                        }
                        return Err(anyhow!("auth failed: {} / {}", status, s2));
                    }
                    if status.as_u16() == 429 || status.is_server_error() {
                        if attempt >= self.max_retries {
                            let txt = r.text().await.unwrap_or_default();
                            return Err(anyhow!("model {} status {} after retries: {}", model.id(), status, truncate(&txt, 300)));
                        }
                        self.backoff(attempt).await;
                        attempt += 1;
                        self.usage.retries.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    let txt = r.text().await.unwrap_or_default();
                    if !status.is_success() {
                        return Err(anyhow!("model {} HTTP {}: {}", model.id(), status, truncate(&txt, 400)));
                    }
                    return self.extract(model, &txt);
                }
                Err(e) => {
                    if attempt >= self.max_retries {
                        return Err(anyhow!("request error for {}: {}", model.id(), e));
                    }
                    self.backoff(attempt).await;
                    attempt += 1;
                    self.usage.retries.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }
        }
    }

    async fn backoff(&self, attempt: u32) {
        // exponential backoff with jitter, capped.
        use rand::Rng;
        let base = 500u64 * (1u64 << attempt.min(5));
        let jitter = rand::thread_rng().gen_range(0..400);
        tokio::time::sleep(Duration::from_millis((base + jitter).min(20_000))).await;
    }

    fn record_usage(&self, v: &Value) {
        if let Some(u) = v.get("usage") {
            let it = u
                .get("input_tokens")
                .or_else(|| u.get("prompt_tokens"))
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            let ot = u
                .get("output_tokens")
                .or_else(|| u.get("completion_tokens"))
                .and_then(|x| x.as_u64())
                .unwrap_or(0);
            self.usage.input_tokens.fetch_add(it, Ordering::Relaxed);
            self.usage.output_tokens.fetch_add(ot, Ordering::Relaxed);
        }
    }

    fn extract(&self, model: Model, txt: &str) -> Result<String> {
        let v: Value = serde_json::from_str(txt)
            .with_context(|| format!("non-JSON response from {}: {}", model.id(), truncate(txt, 300)))?;
        self.record_usage(&v);
        if model.uses_responses() {
            // Prefer a top-level convenience field, else find the message item.
            if let Some(s) = v.get("output_text").and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return Ok(s.to_string());
                }
            }
            if let Some(arr) = v.get("output").and_then(|x| x.as_array()) {
                for item in arr {
                    if item.get("type").and_then(|t| t.as_str()) == Some("message") {
                        if let Some(text) = item
                            .get("content")
                            .and_then(|c| c.as_array())
                            .and_then(|c| c.first())
                            .and_then(|c0| c0.get("text"))
                            .and_then(|t| t.as_str())
                        {
                            return Ok(text.to_string());
                        }
                    }
                }
            }
            // Some responses are flagged incomplete (token budget consumed by reasoning).
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("?");
            Err(anyhow!("no message in /responses output (status={}) for {}", status, model.id()))
        } else {
            v.get("choices")
                .and_then(|c| c.as_array())
                .and_then(|c| c.first())
                .and_then(|c0| c0.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| anyhow!("no choices[0].message.content for {}", model.id()))
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

/// Strip markdown code fences and isolate the first JSON value in a model reply.
pub fn extract_json(text: &str) -> Result<Value> {
    let t = text.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    let t = t.trim();
    // Try whole string, then the first {...} or [...] block.
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        return Ok(v);
    }
    let start = t.find(['{', '[']);
    let end = t.rfind(['}', ']']);
    if let (Some(s), Some(e)) = (start, end) {
        if e > s {
            if let Ok(v) = serde_json::from_str::<Value>(&t[s..=e]) {
                return Ok(v);
            }
        }
    }
    Err(anyhow!("could not parse JSON from model reply: {}", truncate(text, 200)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_parse_and_shape() {
        assert_eq!(Model::parse("gpt-4o").id(), "gpt-4o");
        assert_eq!(Model::parse("GPT-5.5").id(), "gpt-5.5");
        assert_eq!(Model::parse("grok-4.3").id(), "grok-4.3");
        assert!(Model::Gpt4o.uses_responses());
        assert!(Model::Gpt55.uses_responses());
        assert!(!Model::Grok43.uses_responses());
    }

    #[test]
    fn extract_json_strips_fences() {
        let v = extract_json("```json\n{\"a\": 1}\n```").unwrap();
        assert_eq!(v["a"], 1);
        let v2 = extract_json("here you go: [1,2,3] done").unwrap();
        assert_eq!(v2[2], 3);
        let v3 = extract_json("{\"x\": {\"y\": 2}}").unwrap();
        assert_eq!(v3["x"]["y"], 2);
    }

    #[test]
    fn extract_responses_message_skips_reasoning() {
        let client = offline_client();
        let body = serde_json::json!({
            "output": [
                {"type": "reasoning", "content": []},
                {"type": "message", "content": [{"type":"output_text","text":"HELLO"}]}
            ]
        })
        .to_string();
        assert_eq!(client.extract(Model::Gpt55, &body).unwrap(), "HELLO");
    }

    #[test]
    fn extract_chat_completions() {
        let client = offline_client();
        let body = serde_json::json!({
            "choices": [{"message": {"role":"assistant","content":"WORLD"}}]
        })
        .to_string();
        assert_eq!(client.extract(Model::Grok43, &body).unwrap(), "WORLD");
    }

    fn offline_client() -> ModelClient {
        ModelClient {
            http: reqwest::Client::new(),
            base: DEFAULT_BASE.to_string(),
            api_key: String::new(),
            sem: Arc::new(Semaphore::new(1)),
            max_retries: 0,
            cache: None,
            usage: Arc::new(Usage::default()),
            offline: true,
        }
    }
}
