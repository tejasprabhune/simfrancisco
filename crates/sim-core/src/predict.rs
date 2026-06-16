//! Prediction engine: persona + as-of-date + event → weighted opinion / vote / probability.
//!
//! Cost control (BRIEF §5.4): agents are clustered into demographic *archetypes*; one
//! batched LLM call answers ~12 archetypes at once and returns a per-archetype YES
//! probability. Every agent inherits its archetype's probability, then we post-stratify
//! with PUMS weights — a standard synthetic-survey estimator. Aggregation math lives in
//! `aggregate` and is unit-tested. "clean" mode reads persona + broadcast event only.

use crate::agent::Agent;
use crate::aggregate;
use crate::city::CityProfile;
use crate::model::{extract_json, Model, ModelClient};
use crate::persona::Population;
use anyhow::Result;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Framing {
    /// The agent casts their own vote (elections, ballot measures).
    Vote,
    /// The agent forecasts an external event's probability (prediction markets).
    Belief,
    /// The agent picks among N labelled options (multi-candidate markets, lifestyle
    /// and preference questions). Uses Poll.options; result is a distribution.
    Options,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Population0 {
    All,
    CvapLikelyVoter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Clean,
    Social,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Event {
    pub text: String,
    pub as_of_date: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Poll {
    pub question: String,
    /// Neutral factual description of what is being decided (no outcome leakage).
    pub description: String,
    pub framing: Framing,
    pub as_of_date: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub population: Option<String>,
    #[serde(default)]
    pub event: Option<Event>,
    /// For Framing::Options: the labelled choice set. Empty for Vote/Belief (binary).
    #[serde(default)]
    pub options: Vec<String>,
}

impl Poll {
    pub fn model(&self) -> Model {
        // Live default is Claude Sonnet; the rubric pins gpt-4o per entry for the
        // leakage-free 2024 backtest (a later-cutoff model would recall those results).
        Model::parse(self.model.as_deref().unwrap_or("claude-sonnet-4-6"))
    }
    pub fn pop(&self) -> Population0 {
        match self.population.as_deref() {
            Some("cvap_likely_voter") | Some("cvap") => Population0::CvapLikelyVoter,
            _ => Population0::All,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct DemoBreak {
    pub key: String,
    pub yes_share: f64,
    pub weight: f64,
    pub n: usize,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PollResult {
    pub question: String,
    pub as_of_date: String,
    pub model: String,
    pub p_yes: f64,
    pub ci_low: f64,
    pub ci_high: f64,
    pub n_agents: usize,
    pub n_eff: f64,
    pub design_effect: f64,
    pub breakdowns: HashMap<String, Vec<DemoBreak>>,
    pub n_archetypes: usize,
    pub n_llm_calls: usize,
    pub sample_rationales: Vec<String>,
    /// For Framing::Options: weighted probability per option (label, p), summing to 1.
    /// Empty for binary polls (use p_yes).
    #[serde(default)]
    pub p_distribution: Vec<(String, f64)>,
}

/// Likely-voter propensity in [0.05, 0.97] by demographic (BRIEF §4.3). Older,
/// more-educated, higher-income, homeowner, married → higher turnout. Documented and
/// configurable; this is a standard, transparent model, not a tuned fudge factor.
pub fn turnout_propensity(a: &Agent, income_q: usize) -> f64 {
    // Gentle skew: real differential turnout exists but SF's likely-voter electorate is
    // still ~83% Democratic, so an over-aggressive moderate skew biases vote shares low.
    let mut z = 0.4_f64;
    z += ((a.rec.age as f64 - 45.0) / 20.0) * 0.35;
    if a.rec.college_plus() {
        z += 0.30;
    }
    z += (income_q as f64 - 2.0) * 0.10;
    if a.homeowner {
        z += 0.22;
    }
    if a.rec.mar == 1 {
        z += 0.12;
    }
    if a.rec.foreign_born() {
        z -= 0.15;
    }
    let p = 1.0 / (1.0 + (-z).exp());
    p.clamp(0.20, 0.98)
}

struct Cluster {
    rep_idx: usize,
    member_idx: Vec<usize>,
}

/// Cluster agents into archetypes, coarsening the key until under `max_clusters`.
fn cluster_agents(pop: &Population, max_clusters: usize) -> Vec<Cluster> {
    let cutoffs = pop.income_cutoffs;
    for level in 0..4 {
        let mut map: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, a) in pop.agents.iter().enumerate() {
            let key = archetype_key_level(a, &cutoffs, level);
            map.entry(key).or_default().push(i);
        }
        if map.len() <= max_clusters || level == 3 {
            let mut clusters: Vec<Cluster> = map
                .into_iter()
                .map(|(_key, member_idx)| Cluster {
                    rep_idx: member_idx[0],
                    member_idx,
                })
                .collect();
            // Deterministic order (by first-member agent index) so batch composition —
            // and therefore prompts and cache keys — is identical across runs. This is what
            // makes "clean mode" reproducible. HashMap iteration order must not leak in.
            clusters.sort_by_key(|c| c.rep_idx);
            return clusters;
        }
    }
    unreachable!()
}

fn archetype_key_level(a: &Agent, cutoffs: &[f64; 4], level: usize) -> String {
    let q = a.income_quintile(cutoffs);
    match level {
        0 => format!(
            "{}|{}|{}|q{}|{}|{}",
            a.rec.age_band(),
            a.rec.race_eth(),
            a.rec.educ(),
            q,
            if a.homeowner { "own" } else { "rent" },
            if a.rec.is_citizen() { "cit" } else { "non" }
        ),
        1 => format!(
            "{}|{}|{}|{}",
            a.rec.age_band(),
            a.rec.race_eth(),
            a.rec.educ(),
            if a.homeowner { "own" } else { "rent" }
        ),
        2 => format!("{}|{}|{}", a.rec.age_band(), a.rec.race_eth(), a.rec.educ()),
        _ => format!("{}|{}", a.rec.age_band(), a.rec.educ()),
    }
}

#[derive(Clone)]
pub struct Engine {
    pub client: ModelClient,
    pub max_clusters: usize,
    pub batch_size: usize,
}

impl Engine {
    pub fn new(client: ModelClient) -> Self {
        Engine {
            client,
            max_clusters: std::env::var("MAX_CLUSTERS").ok().and_then(|v| v.parse().ok()).unwrap_or(160),
            batch_size: 12,
        }
    }

    fn system_prompt(framing: Framing, profile: &CityProfile) -> String {
        match framing {
            Framing::Vote => profile.vote_prompt(),
            Framing::Belief => profile.belief_prompt(),
            Framing::Options => profile.options_prompt(),
        }
    }

    fn build_batch_prompt(poll: &Poll, profiles: &[(usize, &str)], city_name: &str, news_block: &str) -> String {
        let mut s = String::new();
        s.push_str(&format!("Date (reason as of this date): {}\n", poll.as_of_date));
        if !news_block.is_empty() {
            s.push_str(news_block);
            s.push('\n');
        }
        if let Some(ev) = &poll.event {
            s.push_str(&format!("Recent event everyone is aware of: {}\n", ev.text));
        }
        match poll.framing {
            Framing::Vote => {
                s.push_str(&format!("Ballot question / choice: {}\n", poll.question));
                s.push_str(&format!("What it does (neutral summary): {}\n", poll.description));
                s.push_str("A YES means voting for / in favor.\n\n");
            }
            Framing::Belief => {
                s.push_str(&format!("Event in question: {}\n", poll.question));
                s.push_str(&format!("Context (neutral): {}\n\n", poll.description));
            }
            Framing::Options => {
                s.push_str(&format!("Question: {}\n", poll.question));
                if !poll.description.is_empty() {
                    s.push_str(&format!("Context (neutral): {}\n", poll.description));
                }
                s.push_str("Options (choose among these, in order):\n");
                for (i, o) in poll.options.iter().enumerate() {
                    s.push_str(&format!("  {i}. {o}\n"));
                }
                s.push('\n');
            }
        }
        s.push_str("Resident profiles:\n");
        for (n, (_, persona)) in profiles.iter().enumerate() {
            s.push_str(&format!("{}. {}\n", n + 1, persona));
        }
        if matches!(poll.framing, Framing::Options) {
            let zeros = vec!["0.0"; poll.options.len().max(1)].join(",");
            s.push_str(&format!(
                "\nFor each profile, give the probability THIS resident picks each option (a distribution over the {} options, in order, summing to 1).\n\
Return ONLY a JSON array, one object per profile in order:\n\
[{{\"i\":1,\"dist\":[{zeros}],\"why\":\"<=10 words\"}}, ...]\n\
Ground each distribution in the resident's profile for {city_name}, not stereotypes.",
                poll.options.len(),
            ));
        } else {
            s.push_str(&format!(
                "\nReturn ONLY a JSON array, one object per profile in order:\n\
[{{\"i\":1,\"p_yes\":0.0,\"why\":\"<=10 words\"}}, ...]\n\
p_yes is a probability between 0 and 1. Be realistic and calibrated to {city_name} at that date.",
            ));
        }
        s
    }

    /// Run a poll over a population. Returns weighted result + breakdowns + CI.
    pub async fn run_poll(&self, pop: &Population, poll: &Poll) -> Result<PollResult> {
        let model = poll.model();
        let clusters = cluster_agents(pop, self.max_clusters);
        let cutoffs = pop.income_cutoffs;

        // archetype -> p_yes via batched LLM calls
        let mut p_by_cluster: Vec<f64> = vec![0.5; clusters.len()];
        let mut rationale: Vec<String> = vec![String::new(); clusters.len()];
        let sys = Self::system_prompt(poll.framing, &pop.profile);
        // Inject today's news into LIVE polls only (recent as_of_date); historical
        // backtests keep an old as_of_date and never see it, so they stay leakage-free.
        let news_block = if poll.as_of_date.as_str() >= "2025-06-01" {
            crate::news::prompt_block(&pop.profile.slug)
        } else {
            String::new()
        };
        let n_opts = poll.options.len();
        let is_options = matches!(poll.framing, Framing::Options) && n_opts >= 2;
        let mut dist_by_cluster: Vec<Vec<f64>> =
            vec![vec![1.0 / n_opts.max(1) as f64; n_opts]; clusters.len()];

        let mut calls = 0usize;
        let mut batch_start = 0usize;
        let mut futs = Vec::new();
        while batch_start < clusters.len() {
            let end = (batch_start + self.batch_size).min(clusters.len());
            let profiles: Vec<(usize, String)> = (batch_start..end)
                .map(|ci| (ci, pop.agents[clusters[ci].rep_idx].persona.clone()))
                .collect();
            let prof_refs: Vec<(usize, &str)> =
                profiles.iter().map(|(i, s)| (*i, s.as_str())).collect();
            let user = Self::build_batch_prompt(poll, &prof_refs, &pop.profile.prompt_name, &news_block);
            let client = self.client.clone();
            let sys2 = sys.clone();
            let idxs: Vec<usize> = (batch_start..end).collect();
            futs.push(async move {
                let resp = client.complete(model, &sys2, &user, 1600).await;
                (idxs, resp)
            });
            calls += 1;
            batch_start = end;
        }

        let results = futures::future::join_all(futs).await;
        for (idxs, resp) in results {
            match resp {
                Ok(text) => {
                    if let Ok(v) = extract_json(&text) {
                        if let Some(arr) = v.as_array() {
                            for (k, item) in arr.iter().enumerate() {
                                if k >= idxs.len() {
                                    break;
                                }
                                let ci = idxs[k];
                                if is_options {
                                    if let Some(d) = item.get("dist").and_then(|x| x.as_array()) {
                                        let v: Vec<f64> =
                                            d.iter().filter_map(|x| x.as_f64()).collect();
                                        if v.len() == n_opts {
                                            let s: f64 = v.iter().map(|x| x.max(0.0)).sum();
                                            if s > 0.0 {
                                                dist_by_cluster[ci] =
                                                    v.iter().map(|x| x.max(0.0) / s).collect();
                                            }
                                        }
                                    }
                                } else if let Some(p) = item.get("p_yes").and_then(|x| x.as_f64()) {
                                    p_by_cluster[ci] = p.clamp(0.0, 1.0);
                                }
                                if let Some(w) = item.get("why").and_then(|x| x.as_str()) {
                                    rationale[ci] = w.to_string();
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("poll batch failed: {e}");
                }
            }
        }

        // multi-option framing: aggregate the per-archetype distribution over agents.
        if is_options {
            let cutoffs = pop.income_cutoffs;
            let is_election = matches!(poll.pop(), Population0::CvapLikelyVoter);
            let mut cluster_of: Vec<usize> = vec![0; pop.agents.len()];
            for (ci, c) in clusters.iter().enumerate() {
                for &mi in &c.member_idx {
                    cluster_of[mi] = ci;
                }
            }
            let mut answers: Vec<aggregate::WeightedAnswer> = Vec::new();
            for (i, a) in pop.agents.iter().enumerate() {
                if is_election && !a.rec.is_cvap() {
                    continue;
                }
                let q = a.income_quintile(&cutoffs);
                let w = if is_election { a.weight() * turnout_propensity(a, q) } else { a.weight() };
                answers.push(aggregate::WeightedAnswer {
                    weight: w,
                    probs: dist_by_cluster[cluster_of[i]].clone(),
                });
            }
            let weights: Vec<f64> = answers.iter().map(|x| x.weight).collect();
            let dist = aggregate::weighted_distribution(&answers, n_opts);
            let p_top = dist.iter().cloned().fold(0.0f64, f64::max);
            let p_distribution: Vec<(String, f64)> =
                poll.options.iter().cloned().zip(dist.iter().cloned()).collect();
            let sample_rationales: Vec<String> =
                rationale.iter().filter(|r| !r.is_empty()).take(8).cloned().collect();
            return Ok(PollResult {
                question: poll.question.clone(),
                as_of_date: poll.as_of_date.clone(),
                model: model.id().to_string(),
                p_yes: p_top,
                ci_low: 0.0,
                ci_high: 0.0,
                n_agents: answers.len(),
                n_eff: aggregate::effective_n(&weights),
                design_effect: aggregate::design_effect(&weights),
                breakdowns: HashMap::new(),
                n_archetypes: clusters.len(),
                n_llm_calls: calls,
                sample_rationales,
                p_distribution,
            });
        }

        // map cluster p_yes onto agents and build weighted (w, p) rows for the population
        let mut agent_p: Vec<f64> = vec![0.5; pop.agents.len()];
        for (ci, c) in clusters.iter().enumerate() {
            for &mi in &c.member_idx {
                agent_p[mi] = p_by_cluster[ci];
            }
        }

        let is_election = matches!(poll.pop(), Population0::CvapLikelyVoter);
        let mut rows: Vec<(f64, f64)> = Vec::new();
        let mut breakdown_rows: HashMap<&'static str, Vec<(String, f64, f64)>> = HashMap::new();
        let dims = ["age", "race", "educ", "income_q", "puma", "tenure"];
        for d in dims {
            breakdown_rows.insert(d, Vec::new());
        }
        for (i, a) in pop.agents.iter().enumerate() {
            if is_election && !a.rec.is_cvap() {
                continue;
            }
            let q = a.income_quintile(&cutoffs);
            let w = if is_election {
                a.weight() * turnout_propensity(a, q)
            } else {
                a.weight()
            };
            let p = agent_p[i];
            rows.push((w, p));
            breakdown_rows.get_mut("age").unwrap().push((a.rec.age_band().to_string(), w, p));
            breakdown_rows.get_mut("race").unwrap().push((a.rec.race_eth().to_string(), w, p));
            breakdown_rows.get_mut("educ").unwrap().push((a.rec.educ().to_string(), w, p));
            breakdown_rows.get_mut("income_q").unwrap().push((format!("q{q}"), w, p));
            breakdown_rows.get_mut("puma").unwrap().push((a.rec.puma.to_string(), w, p));
            breakdown_rows.get_mut("tenure").unwrap().push((if a.homeowner { "own".into() } else { "rent".into() }, w, p));
        }

        let p_yes = aggregate::weighted_yes_share(&rows);
        let weights: Vec<f64> = rows.iter().map(|r| r.0).collect();
        let (ci_low, ci_high) = aggregate::weighted_bootstrap_ci(&rows, 400, 0.05, pop.seed ^ 0x9e3779b9);
        let mut breakdowns: HashMap<String, Vec<DemoBreak>> = HashMap::new();
        for (d, rws) in breakdown_rows {
            let b = aggregate::breakdown(&rws);
            breakdowns.insert(
                d.to_string(),
                b.into_iter()
                    .map(|(k, ys, w, n)| DemoBreak { key: k, yes_share: ys, weight: w, n })
                    .collect(),
            );
        }

        let sample_rationales: Vec<String> = rationale
            .iter()
            .filter(|r| !r.is_empty())
            .take(8)
            .cloned()
            .collect();

        Ok(PollResult {
            question: poll.question.clone(),
            as_of_date: poll.as_of_date.clone(),
            model: model.id().to_string(),
            p_yes,
            ci_low,
            ci_high,
            n_agents: rows.len(),
            n_eff: aggregate::effective_n(&weights),
            design_effect: aggregate::design_effect(&weights),
            breakdowns,
            n_archetypes: clusters.len(),
            n_llm_calls: calls,
            sample_rationales,
            p_distribution: Vec::new(),
        })
    }

    /// Counterfactual: poll baseline vs poll-with-event, return (baseline, with_event, delta).
    pub async fn run_counterfactual(
        &self,
        pop: &Population,
        base: &Poll,
        event: Event,
    ) -> Result<(PollResult, PollResult, f64)> {
        let baseline = self.run_poll(pop, base).await?;
        let mut withev = base.clone();
        withev.event = Some(event);
        let after = self.run_poll(pop, &withev).await?;
        let delta = after.p_yes - baseline.p_yes;
        Ok((baseline, after, delta))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona::build_population;
    use crate::pums::PumsRecord;

    fn rec(age: u8, schl: u8, povpip: f64) -> PumsRecord {
        PumsRecord {
            serialno: "x".into(), sporder: 1, pwgtp: 10.0, age, sex: 1, rac1p: 1, hisp: 1, schl,
            pincp: povpip, povpip, occp: 1020, cow: 1, esr: 1, cit: 1, mar: 5, nativity: 1,
            puma: 7510, adjinc: 1.0,
        }
    }

    #[test]
    fn turnout_increases_with_age_and_education() {
        let recs: Vec<PumsRecord> = (0..50).map(|_| rec(70, 22, 200000.0)).collect();
        let pop = build_population(&recs, 50, 1, None);
        let q = 4;
        let old_grad = turnout_propensity(&pop.agents[0], q);
        let recs2: Vec<PumsRecord> = (0..50).map(|_| rec(20, 16, 20000.0)).collect();
        let pop2 = build_population(&recs2, 50, 1, None);
        let young_hs = turnout_propensity(&pop2.agents[0], 0);
        assert!(old_grad > young_hs, "{old_grad} vs {young_hs}");
        assert!(old_grad <= 0.97 && young_hs >= 0.05);
    }

    #[test]
    fn clustering_bounds_count() {
        let recs: Vec<PumsRecord> = (0..2000)
            .map(|i| rec(18 + (i % 70) as u8, 16 + (i % 9) as u8, 20000.0 + (i as f64) * 500.0))
            .collect();
        let pop = build_population(&recs, 1500, 42, None);
        let clusters = cluster_agents(&pop, 80);
        assert!(clusters.len() <= 80, "got {}", clusters.len());
        let total: usize = clusters.iter().map(|c| c.member_idx.len()).sum();
        assert_eq!(total, 1500);
    }
}
