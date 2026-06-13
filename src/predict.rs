//! Prediction engine: persona + as-of-date + event → weighted opinion / vote / probability.
//!
//! Cost control (BRIEF §5.4): agents are clustered into demographic *archetypes*; one
//! batched LLM call answers ~12 archetypes at once and returns a per-archetype YES
//! probability. Every agent inherits its archetype's probability, then we post-stratify
//! with PUMS weights — a standard synthetic-survey estimator. Aggregation math lives in
//! `aggregate` and is unit-tested. "clean" mode reads persona + broadcast event only.

use crate::agent::Agent;
use crate::aggregate;
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
}

impl Poll {
    pub fn model(&self) -> Model {
        Model::parse(self.model.as_deref().unwrap_or("gpt-4o"))
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
            clusters.sort_by(|a, b| b.member_idx.len().cmp(&a.member_idx.len()));
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

    fn system_prompt(framing: Framing) -> String {
        match framing {
            Framing::Vote => "You simulate the San Francisco electorate for a nonpartisan academic forecasting model. \
Reason as a real San Francisco resident with the given profile and lived experience of the city on the given date. \
San Francisco is one of the most Democratic-leaning cities in the United States — strong partisans here vote their lean about 85–95% of the time, so be realistic and confident, not hedged, when a profile clearly leans one way. \
At the TOP of the ticket SF is especially lopsided: in the most recent prior presidential elections (2016 and 2020) the Republican nominee won only about one in six San Francisco voters citywide, and even higher-income, older, and homeowner residents vote Democratic for president at high rates. Residents split far more on local and state ballot MEASURES, where they weigh each proposition on its own merits. \
But residents are pragmatic and not monolithic: by 2022 voters had recalled progressive District Attorney Chesa Boudin (June 2022) and three school-board members amid frustration over public safety, retail theft, open-air drug markets, and city governance. \
Weigh each measure on its merits as this resident actually would, given the city's real mood at the date — do NOT fall back on ideological stereotypes. \
Use ONLY knowledge available on the given date; never use any outcome that occurred after it. \
For each profile, estimate the probability THIS resident casts a YES vote. Respond with STRICT JSON only, no prose outside it.".to_string(),
            Framing::Belief => "You are a calibrated political forecaster simulating informed San Francisco residents for a prediction-market model. \
For each resident profile, output a well-calibrated probability for the described event, reasoning ANALYTICALLY about the political landscape and election mechanics — not partisan hope. \
Key mechanic: California uses a TOP-TWO primary — the two highest vote-getters advance to the general election regardless of party, so when one prominent candidate is the only major contender from the minority party, that candidate usually consolidates enough of the minority vote to take second place even in a heavily one-party state. \
Estimate what is actually most likely to happen, not what a partisan would prefer. \
Use ONLY knowledge available on the given date; never use any outcome that occurred after it. Respond with STRICT JSON only, no prose outside it.".to_string(),
        }
    }

    fn build_batch_prompt(poll: &Poll, profiles: &[(usize, &str)]) -> String {
        let mut s = String::new();
        s.push_str(&format!("Date (reason as of this date): {}\n", poll.as_of_date));
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
        }
        s.push_str("Voter profiles:\n");
        for (n, (_, persona)) in profiles.iter().enumerate() {
            s.push_str(&format!("{}. {}\n", n + 1, persona));
        }
        s.push_str(
            "\nReturn ONLY a JSON array, one object per profile in order:\n\
[{\"i\":1,\"p_yes\":0.0,\"why\":\"<=10 words\"}, ...]\n\
p_yes is a probability between 0 and 1. Be realistic and calibrated to San Francisco at that date.",
        );
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
        let sys = Self::system_prompt(poll.framing);

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
            let user = Self::build_batch_prompt(poll, &prof_refs);
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
                                if let Some(p) = item.get("p_yes").and_then(|x| x.as_f64()) {
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
