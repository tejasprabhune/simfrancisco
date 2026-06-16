//! Weighted aggregation math for poll results (the scored core).
//!
//! Every population estimate uses PUMS person weights `w_i`. For an option k the
//! estimate is the Horvitz-Thompson style ratio
//!     p_hat(k) = Σ_i w_i · a_i(k)  /  Σ_i w_i
//! where `a_i(k)` is agent i's answer indicator for option k. We allow a *soft*
//! indicator in [0,1] (an archetype's conditional probability of choosing k); the
//! hard 0/1 individual case is the special case. CIs use a weighted bootstrap that
//! is design-effect aware (heavily-weighted agents swing resamples appropriately).

use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

/// One agent's (or archetype's) answer: a survey weight and a probability mass
/// over the option set. For a binary yes/no, `probs = [p_no, p_yes]` or `[p_yes]`
/// depending on caller convention; helpers below fix the convention explicitly.
#[derive(Clone, Debug)]
pub struct WeightedAnswer {
    pub weight: f64,
    pub probs: Vec<f64>,
}

impl WeightedAnswer {
    pub fn hard(weight: f64, option: usize, n_options: usize) -> Self {
        let mut probs = vec![0.0; n_options];
        if option < n_options {
            probs[option] = 1.0;
        }
        WeightedAnswer { weight, probs }
    }
}

/// Weighted distribution over `n_options`. Returns a vector that sums to 1
/// (unless total weight is zero, in which case a uniform vector is returned).
pub fn weighted_distribution(answers: &[WeightedAnswer], n_options: usize) -> Vec<f64> {
    let mut num = vec![0.0f64; n_options];
    let mut den = 0.0f64;
    for a in answers {
        den += a.weight;
        for k in 0..n_options {
            let p = a.probs.get(k).copied().unwrap_or(0.0);
            num[k] += a.weight * p;
        }
    }
    if den <= 0.0 {
        return vec![1.0 / n_options as f64; n_options];
    }
    num.iter().map(|x| x / den).collect()
}

/// Weighted yes-share given (weight, p_yes) pairs. p_yes is a soft indicator in [0,1].
pub fn weighted_yes_share(answers: &[(f64, f64)]) -> f64 {
    let mut num = 0.0;
    let mut den = 0.0;
    for (w, p) in answers {
        num += w * p.clamp(0.0, 1.0);
        den += w;
    }
    if den <= 0.0 {
        0.0
    } else {
        num / den
    }
}

/// Kish effective sample size: (Σw)² / Σw². Equals n when weights are equal.
pub fn effective_n(weights: &[f64]) -> f64 {
    let s: f64 = weights.iter().sum();
    let s2: f64 = weights.iter().map(|w| w * w).sum();
    if s2 <= 0.0 {
        0.0
    } else {
        s * s / s2
    }
}

/// Design effect (Kish): n / n_eff = n · Σw² / (Σw)².
pub fn design_effect(weights: &[f64]) -> f64 {
    let n = weights.len() as f64;
    let neff = effective_n(weights);
    if neff <= 0.0 {
        1.0
    } else {
        n / neff
    }
}

/// Weighted bootstrap CI for the yes-share. Resamples agents uniformly with
/// replacement `b` times, recomputing the weighted share each time, then takes
/// percentiles. Deterministic given `seed`.
pub fn weighted_bootstrap_ci(answers: &[(f64, f64)], b: usize, alpha: f64, seed: u64) -> (f64, f64) {
    if answers.is_empty() {
        return (0.0, 0.0);
    }
    let n = answers.len();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut shares = Vec::with_capacity(b);
    for _ in 0..b {
        let mut num = 0.0;
        let mut den = 0.0;
        for _ in 0..n {
            // inline uniform index draw
            let idx = (next_u64(&mut rng) % n as u64) as usize;
            let (w, p) = answers[idx];
            num += w * p.clamp(0.0, 1.0);
            den += w;
        }
        shares.push(if den > 0.0 { num / den } else { 0.0 });
    }
    shares.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let lo_idx = ((alpha / 2.0) * b as f64).floor() as usize;
    let hi_idx = (((1.0 - alpha / 2.0) * b as f64).ceil() as usize).saturating_sub(1);
    (
        shares[lo_idx.min(b - 1)],
        shares[hi_idx.min(b - 1)],
    )
}

fn next_u64(rng: &mut ChaCha8Rng) -> u64 {
    use rand::RngCore;
    rng.next_u64()
}

/// Group answers by a key (e.g. age band) and compute the weighted yes-share for
/// each group, returning (key, yes_share, group_weight, n).
pub fn breakdown<K: Eq + std::hash::Hash + Clone>(
    rows: &[(K, f64, f64)],
) -> Vec<(K, f64, f64, usize)> {
    use std::collections::HashMap;
    let mut acc: HashMap<K, (f64, f64, usize)> = HashMap::new();
    for (k, w, p) in rows {
        let e = acc.entry(k.clone()).or_insert((0.0, 0.0, 0));
        e.0 += w * p.clamp(0.0, 1.0);
        e.1 += w;
        e.2 += 1;
    }
    let mut out: Vec<(K, f64, f64, usize)> = acc
        .into_iter()
        .map(|(k, (num, den, n))| (k, if den > 0.0 { num / den } else { 0.0 }, den, n))
        .collect();
    out
        .sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// L1 (total variation ×½) distance between two distributions over the same support.
pub fn tv_distance(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().max(b.len());
    let mut d = 0.0;
    for i in 0..n {
        let ai = a.get(i).copied().unwrap_or(0.0);
        let bi = b.get(i).copied().unwrap_or(0.0);
        d += (ai - bi).abs();
    }
    d / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_weights_is_plain_mean() {
        // 3 yes, 1 no, equal weights -> 0.75
        let ans = vec![(1.0, 1.0), (1.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
        assert!((weighted_yes_share(&ans) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn weights_change_the_estimate() {
        // The single "no" agent carries 9x weight -> should pull share down hard.
        let ans = vec![(1.0, 1.0), (1.0, 1.0), (1.0, 1.0), (9.0, 0.0)];
        let s = weighted_yes_share(&ans);
        assert!((s - 0.25).abs() < 1e-12, "got {s}");
    }

    #[test]
    fn soft_indicators_average() {
        // Two archetypes: one 80% yes weight 10, one 20% yes weight 10 -> 0.5
        let ans = vec![(10.0, 0.8), (10.0, 0.2)];
        assert!((weighted_yes_share(&ans) - 0.5).abs() < 1e-12);
        // Reweight 3:1 -> (3*0.8 + 1*0.2)/4 = 0.65
        let ans2 = vec![(30.0, 0.8), (10.0, 0.2)];
        assert!((weighted_yes_share(&ans2) - 0.65).abs() < 1e-12);
    }

    #[test]
    fn distribution_sums_to_one() {
        let answers = vec![
            WeightedAnswer::hard(2.0, 0, 3),
            WeightedAnswer::hard(1.0, 1, 3),
            WeightedAnswer::hard(1.0, 2, 3),
            WeightedAnswer { weight: 4.0, probs: vec![0.5, 0.25, 0.25] },
        ];
        let d = weighted_distribution(&answers, 3);
        let sum: f64 = d.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "dist {d:?}");
        // option 0 weight: 2*1 + 4*0.5 = 4 ; total weight 8 -> 0.5
        assert!((d[0] - 0.5).abs() < 1e-9, "{d:?}");
    }

    #[test]
    fn design_effect_equal_weights_is_one() {
        let w = vec![1.0; 100];
        assert!((design_effect(&w) - 1.0).abs() < 1e-9);
        assert!((effective_n(&w) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn design_effect_unequal_greater_than_one() {
        let mut w = vec![1.0; 99];
        w.push(100.0);
        assert!(design_effect(&w) > 1.0);
        assert!(effective_n(&w) < 100.0);
    }

    #[test]
    fn bootstrap_ci_brackets_point_estimate() {
        // 70% yes over 200 agents, equal weights.
        let mut ans = Vec::new();
        for i in 0..200 {
            ans.push((1.0, if i < 140 { 1.0 } else { 0.0 }));
        }
        let point = weighted_yes_share(&ans);
        let (lo, hi) = weighted_bootstrap_ci(&ans, 500, 0.05, 7);
        assert!(lo < point && point < hi, "lo {lo} point {point} hi {hi}");
        assert!(hi - lo < 0.20, "CI too wide: {lo}..{hi}");
        // determinism
        let (lo2, hi2) = weighted_bootstrap_ci(&ans, 500, 0.05, 7);
        assert_eq!((lo, hi), (lo2, hi2));
    }

    #[test]
    fn breakdown_groups_correctly() {
        let rows = vec![
            ("young", 1.0, 1.0),
            ("young", 1.0, 0.0),
            ("old", 2.0, 1.0),
            ("old", 2.0, 1.0),
        ];
        let b = breakdown(&rows);
        let young = b.iter().find(|(k, ..)| *k == "young").unwrap();
        let old = b.iter().find(|(k, ..)| *k == "old").unwrap();
        assert!((young.1 - 0.5).abs() < 1e-9);
        assert!((old.1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tv_distance_basic() {
        assert!((tv_distance(&[0.5, 0.5], &[0.5, 0.5])).abs() < 1e-12);
        assert!((tv_distance(&[1.0, 0.0], &[0.0, 1.0]) - 1.0).abs() < 1e-12);
    }
}
