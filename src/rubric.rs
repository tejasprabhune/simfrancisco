//! Rubric parsing + scoring (the machine-checkable "done").
//!
//! `validate` loads `rubric.yaml`, runs each entry's poll in clean mode at its
//! as_of_date/model, and scores: elections → abs error on vote share; markets →
//! Brier vs resolved outcome (informative bucket only counts); counterfactuals →
//! correct direction (+ plausible magnitude). Exit 0 iff the weighted score clears
//! `thresholds.weighted_score_min`. Targets are fixed public ground truth and must
//! never be tuned to the model.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Rubric {
    pub meta: Meta,
    pub weights: Weights,
    pub thresholds: Thresholds,
    #[serde(default)]
    pub elections_measures: Vec<ElectionEntry>,
    #[serde(default)]
    pub resolved_markets: ResolvedMarkets,
    #[serde(default)]
    pub counterfactuals: Vec<CfEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    pub validation_n: usize,
    pub default_seed: u64,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Weights {
    pub elections_measures: f64,
    pub resolved_markets_sf_informative: f64,
    #[serde(default)]
    pub resolved_markets_general: f64,
    #[serde(default)]
    pub live_markets: f64,
    pub counterfactuals: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Thresholds {
    pub weighted_score_min: f64,
    pub elections_measures_max_abs_err: f64,
    pub resolved_markets_max_brier: f64,
    pub counterfactual_direction_min: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ElectionEntry {
    pub id: String,
    pub as_of_date: String,
    pub model: String,
    pub population: String,
    pub question: String,
    pub description: String,
    pub target_share: f64,
    pub tolerance: f64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResolvedMarkets {
    #[serde(default)]
    pub sf_opinion_informative: Vec<MarketEntry>,
    #[serde(default)]
    pub general_knowledge: Vec<MarketEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MarketEntry {
    pub id: String,
    #[serde(default)]
    pub source: String,
    pub as_of_date: String,
    pub model: String,
    pub question: String,
    pub description: String,
    pub outcome: f64, // 1 = happened, 0 = not
}

#[derive(Debug, Clone, Deserialize)]
pub struct CfEntry {
    pub id: String,
    pub as_of_date: String,
    pub model: String,
    #[serde(default)]
    pub population: Option<String>,
    #[serde(default = "default_framing")]
    pub framing: String,
    pub question: String,
    pub description: String,
    pub event: String,
    pub expected_direction: String, // up | down
    #[serde(default)]
    pub real_poll_delta: Option<f64>,
    #[serde(default)]
    pub magnitude_tolerance: Option<f64>,
}

fn default_framing() -> String {
    "vote".to_string()
}

impl Rubric {
    pub fn load(path: &str) -> anyhow::Result<Rubric> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }
}

// ---- scoring (pure functions; predicted values come from the prediction engine) ----

pub fn election_entry_score(p_hat: f64, target: f64, tolerance: f64) -> (f64, f64, bool) {
    let abs_err = (p_hat - target).abs();
    // score 1.0 at zero error, 0.5 at the tolerance, 0.0 at 2× tolerance.
    let score = (1.0 - abs_err / (2.0 * tolerance)).clamp(0.0, 1.0);
    let pass = abs_err <= tolerance + 1e-9; // boundary-inclusive
    (score, abs_err, pass)
}

pub fn brier(p_hat: f64, outcome: f64) -> f64 {
    (p_hat - outcome).powi(2)
}

pub fn market_entry_score(p_hat: f64, outcome: f64, max_brier: f64) -> (f64, f64, bool) {
    let b = brier(p_hat, outcome);
    // score 1.0 at brier 0, 0.5 at max_brier, 0 at 2× max_brier.
    let score = (1.0 - b / (2.0 * max_brier)).clamp(0.0, 1.0);
    let pass = b <= max_brier + 1e-9;
    (score, b, pass)
}

/// Counterfactual: direction correct? + magnitude plausibility if a real delta exists.
pub fn cf_entry_score(delta: f64, expected_up: bool, real_delta: Option<f64>, mag_tol: f64) -> (f64, bool) {
    let dir_ok = if expected_up { delta > 0.005 } else { delta < -0.005 };
    let mut score = if dir_ok { 1.0 } else { 0.0 };
    if dir_ok {
        if let Some(rd) = real_delta {
            // penalize magnitude error up to half the score
            let mag_err = (delta.abs() - rd.abs()).abs();
            let mag_pen = (mag_err / mag_tol.max(1e-6)).min(1.0) * 0.5;
            score -= mag_pen;
        }
    }
    (score.clamp(0.0, 1.0), dir_ok)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct CategoryScore {
    pub name: String,
    pub score: f64,
    pub weight: f64,
    pub n: usize,
    pub passed: bool,
}

/// Combine category scores into the weighted headline. Categories with weight 0 are
/// reported but never affect the headline (general/live market buckets).
pub fn weighted_headline(categories: &[CategoryScore]) -> f64 {
    let mut num = 0.0;
    let mut den = 0.0;
    for c in categories {
        if c.weight > 0.0 {
            num += c.weight * c.score;
            den += c.weight;
        }
    }
    if den <= 0.0 {
        0.0
    } else {
        num / den
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn election_scoring_curve() {
        let (s_exact, e, pass) = election_entry_score(0.84, 0.84, 0.05);
        assert!((s_exact - 1.0).abs() < 1e-9 && e < 1e-9 && pass);
        let (s_tol, _, pass2) = election_entry_score(0.89, 0.84, 0.05);
        assert!((s_tol - 0.5).abs() < 1e-6 && pass2); // at tolerance: half score, still passes
        let (s_bad, _, pass3) = election_entry_score(0.95, 0.84, 0.05);
        assert!(s_bad < 0.2 && !pass3);
    }

    #[test]
    fn brier_and_market_scoring() {
        assert!((brier(1.0, 1.0)).abs() < 1e-9);
        assert!((brier(0.0, 1.0) - 1.0).abs() < 1e-9);
        let (score, b, pass) = market_entry_score(0.85, 1.0, 0.18);
        assert!(b < 0.18 && pass && score > 0.9);
        let (s2, _, pass2) = market_entry_score(0.4, 1.0, 0.18);
        assert!(!pass2 && s2 < 0.1);
    }

    #[test]
    fn counterfactual_direction() {
        let (s, ok) = cf_entry_score(0.06, true, None, 0.1);
        assert!(ok && (s - 1.0).abs() < 1e-9);
        let (s2, ok2) = cf_entry_score(-0.06, true, None, 0.1);
        assert!(!ok2 && s2 < 1e-9);
        // magnitude penalty when far from real delta
        let (s3, ok3) = cf_entry_score(0.30, true, Some(0.05), 0.10);
        assert!(ok3 && s3 < 1.0);
    }

    #[test]
    fn weighting_headline_respects_weights_and_zeros() {
        let cats = vec![
            CategoryScore { name: "elections".into(), score: 0.6, weight: 1.0, n: 5, passed: true },
            CategoryScore { name: "markets".into(), score: 0.8, weight: 1.5, n: 3, passed: true },
            CategoryScore { name: "cf".into(), score: 1.0, weight: 1.0, n: 2, passed: true },
            CategoryScore { name: "general".into(), score: 0.0, weight: 0.0, n: 1, passed: true },
        ];
        let h = weighted_headline(&cats);
        // (1*0.6 + 1.5*0.8 + 1*1.0) / 3.5 = 2.8/3.5 = 0.8
        assert!((h - 0.8).abs() < 1e-9, "headline {h}");
        // zero-weight 'general' (even at score 0) must not drag the headline.
    }
}
