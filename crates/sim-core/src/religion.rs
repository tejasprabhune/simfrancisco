//! Religion layer. Census has no religion field, so we layer it stochastically from
//! Pew's Religious Landscape Study for the San Francisco metro, conditioned on the
//! agent's demographics. Deterministic given the agent seed.
//!
//! Pew SF-metro baseline (2023-24): Christian 45% (Catholic 24, Protestant 19,
//! other-Christian 2), Unaffiliated 42%, Hindu 4%, Buddhist 3%, Jewish 2%, Muslim 1%,
//! other 2%. SF is among the most secular US metros; the unaffiliated share rises with
//! youth and education. Source: pewresearch.org Religious Landscape Study, SF metro.

use crate::pums::PumsRecord;
use rand::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Religion {
    Catholic,
    Protestant,
    OtherChristian,
    Unaffiliated,
    Jewish,
    Muslim,
    Buddhist,
    Hindu,
    Other,
}

impl Religion {
    pub fn label(&self) -> &'static str {
        match self {
            Religion::Catholic => "Catholic",
            Religion::Protestant => "Protestant",
            Religion::OtherChristian => "other Christian",
            Religion::Unaffiliated => "religiously unaffiliated",
            Religion::Jewish => "Jewish",
            Religion::Muslim => "Muslim",
            Religion::Buddhist => "Buddhist",
            Religion::Hindu => "Hindu",
            Religion::Other => "other faith",
        }
    }
    pub fn all() -> [Religion; 9] {
        [
            Religion::Catholic,
            Religion::Protestant,
            Religion::OtherChristian,
            Religion::Unaffiliated,
            Religion::Jewish,
            Religion::Muslim,
            Religion::Buddhist,
            Religion::Hindu,
            Religion::Other,
        ]
    }
}

/// Conditional probability weights over religions for an agent, then a deterministic draw.
pub fn assign(rec: &PumsRecord, rng: &mut impl Rng, weights: &[f64; 9]) -> Religion {
    // baseline weights (Pew metro), order matches Religion::all()
    let mut w = *weights;

    // race/ethnicity tilts
    match rec.race_eth() {
        "hispanic" => {
            w[0] *= 2.6; // Catholic
            w[1] *= 1.2;
            w[3] *= 0.6;
        }
        "black" => {
            w[1] *= 2.8; // (historically Black) Protestant
            w[3] *= 0.7;
        }
        "asian" => {
            w[6] *= 2.5; // Buddhist
            w[7] *= 1.2;
            w[0] *= 1.1;
            w[3] *= 1.0;
        }
        "white" => {
            w[3] *= 1.15; // more unaffiliated
            w[4] *= 1.6; // Jewish skew among white SF
        }
        _ => {}
    }
    if rec.foreign_born() {
        w[5] *= 3.0; // Muslim
        w[7] *= 2.0; // Hindu
        w[0] *= 1.3; // Catholic (immigrant)
        w[3] *= 0.7;
    }

    // age tilt: younger -> more unaffiliated; older -> more affiliated
    let age = rec.age as f64;
    let youth = ((45.0 - age) / 30.0).clamp(-1.0, 1.0); // +1 young, -1 old
    w[3] *= 1.0 + 0.35 * youth;
    for i in [0usize, 1, 2] {
        w[i] *= 1.0 - 0.18 * youth;
    }

    // education tilt: college+ -> more unaffiliated
    if rec.college_plus() {
        w[3] *= 1.2;
        w[1] *= 0.8;
    }

    // normalize and draw
    let total: f64 = w.iter().sum();
    let mut r = rng.gen::<f64>() * total;
    let all = Religion::all();
    for (i, weight) in w.iter().enumerate() {
        r -= weight;
        if r <= 0.0 {
            return all[i];
        }
    }
    Religion::Unaffiliated
}

/// A baseline "religiosity" intensity in [0,1] used by the value vector.
pub fn religiosity(rel: Religion, rng: &mut impl Rng) -> f64 {
    let base: f64 = match rel {
        Religion::Unaffiliated => 0.08,
        Religion::Jewish => 0.35,
        Religion::Buddhist => 0.4,
        Religion::OtherChristian | Religion::Other => 0.55,
        Religion::Catholic | Religion::Protestant => 0.6,
        Religion::Muslim | Religion::Hindu => 0.7,
    };
    (base + rng.gen_range(-0.15f64..0.15)).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    // SF Pew baseline weights for tests.
    const SFW: [f64; 9] = [0.24, 0.19, 0.02, 0.42, 0.02, 0.01, 0.03, 0.04, 0.03];

    fn rec(race_rac1p: u8, hisp: u16, age: u8) -> PumsRecord {
        PumsRecord {
            serialno: "x".into(), sporder: 1, pwgtp: 10.0, age, sex: 1, rac1p: race_rac1p,
            hisp, schl: 21, pincp: 50000.0, povpip: 350.0, occp: 0, cow: 1, esr: 1, cit: 1,
            mar: 5, nativity: 1, puma: 7510, adjinc: 1.0,
        }
    }

    #[test]
    fn deterministic_given_seed() {
        let r = rec(1, 1, 30);
        let a = assign(&r, &mut ChaCha8Rng::seed_from_u64(5), &SFW);
        let b = assign(&r, &mut ChaCha8Rng::seed_from_u64(5), &SFW);
        assert_eq!(a, b);
    }

    #[test]
    fn hispanic_skews_catholic_in_aggregate() {
        // Over many draws, Hispanic agents are more Catholic than white agents.
        let mut cath_h = 0;
        let mut cath_w = 0;
        for s in 0..2000u64 {
            let h = assign(&rec(8, 2, 40), &mut ChaCha8Rng::seed_from_u64(s), &SFW);
            let w = assign(&rec(1, 1, 40), &mut ChaCha8Rng::seed_from_u64(s), &SFW);
            if h == Religion::Catholic {
                cath_h += 1;
            }
            if w == Religion::Catholic {
                cath_w += 1;
            }
        }
        assert!(cath_h > cath_w, "hispanic {cath_h} vs white {cath_w}");
    }

    #[test]
    fn unaffiliated_is_plurality_overall() {
        let mut unaff = 0;
        for s in 0..3000u64 {
            let r = rec(1, 1, 35);
            if assign(&r, &mut ChaCha8Rng::seed_from_u64(s), &SFW) == Religion::Unaffiliated {
                unaff += 1;
            }
        }
        assert!(unaff as f64 / 3000.0 > 0.35, "unaff frac {}", unaff as f64 / 3000.0);
    }
}
