//! Agents and their value vectors.
//!
//! An agent pairs a real PUMS microdata record (joint demographics + survey weight)
//! with a deterministically generated persona and a compact *value vector* (economic
//! L/R, social L/R, institutional trust, change-vs-status-quo, plus issue saliences).
//! The value vector is what events and debates shift and what cheap aggregations read.

use crate::geo::Cell;
use crate::pums::PumsRecord;
use crate::religion::Religion;

/// Compact, mutable opinion state. Axes are in [-1, 1]; saliences in [0, 1].
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ValueVector {
    pub economic: f64, // -1 economic-left … +1 economic-right
    pub social: f64,   // -1 socially-progressive … +1 socially-conservative
    pub trust: f64,    // -1 distrusts institutions … +1 trusts
    pub change: f64,   // -1 status-quo … +1 pro-change
    // issue salience weights (how much the agent cares)
    pub s_housing: f64,
    pub s_crime: f64,
    pub s_homeless: f64,
    pub s_cost: f64,
    pub s_environment: f64,
    pub s_immigration: f64,
}

impl ValueVector {
    pub fn clamp(&mut self) {
        for v in [
            &mut self.economic,
            &mut self.social,
            &mut self.trust,
            &mut self.change,
        ] {
            *v = v.clamp(-1.0, 1.0);
        }
        for v in [
            &mut self.s_housing,
            &mut self.s_crime,
            &mut self.s_homeless,
            &mut self.s_cost,
            &mut self.s_environment,
            &mut self.s_immigration,
        ] {
            *v = v.clamp(0.0, 1.0);
        }
    }
    /// A short natural-language summary of the leanings, for prompts.
    pub fn describe(&self) -> String {
        let econ = axis_word(self.economic, "economically progressive/redistributionist", "economically moderate", "economically conservative/pro-market");
        let soc = axis_word(self.social, "socially very progressive", "socially moderate", "socially conservative");
        let trust = axis_word(self.trust, "distrustful of government and institutions", "ambivalent about institutions", "trusting of institutions");
        let chg = axis_word(self.change, "prefers stability and incremental change", "open to some change", "wants big structural change");
        let mut top = [
            ("housing/development", self.s_housing),
            ("public safety/crime", self.s_crime),
            ("homelessness", self.s_homeless),
            ("cost of living", self.s_cost),
            ("climate/environment", self.s_environment),
            ("immigration", self.s_immigration),
        ];
        top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        format!(
            "{econ}; {soc}; {trust}; {chg}. Cares most about {} and {}.",
            top[0].0, top[1].0
        )
    }
}

fn axis_word(v: f64, low: &'static str, mid: &'static str, high: &'static str) -> &'static str {
    if v < -0.33 {
        low
    } else if v > 0.33 {
        high
    } else {
        mid
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    pub id: u32,
    pub rec: PumsRecord,
    pub seed: u64,
    pub name: String,
    pub religion: Religion,
    pub religiosity: f64,
    pub homeowner: bool,
    pub values: ValueVector,
    pub persona: String,
    pub occupation: String,
    pub neighborhood: String,
    pub home: Cell,
    pub work: Option<Cell>,
}

impl Agent {
    pub fn weight(&self) -> f64 {
        self.rec.pwgtp
    }
    /// Income quintile (0..4) is assigned at population build time; stored on the record's
    /// derived helpers is not possible, so we recompute against provided cutoffs.
    pub fn income_quintile(&self, cutoffs: &[f64; 4]) -> usize {
        let inc = self.rec.econ_rank();
        cutoffs.iter().filter(|&&c| inc > c).count()
    }
    /// Archetype key for clustering (politically-salient dims). Income quintile needs cutoffs.
    pub fn archetype_key(&self, cutoffs: &[f64; 4]) -> String {
        format!(
            "{}|{}|{}|q{}|{}|{}",
            self.rec.age_band(),
            self.rec.race_eth(),
            self.rec.educ(),
            self.income_quintile(cutoffs),
            if self.homeowner { "own" } else { "rent" },
            if self.rec.is_citizen() { "cit" } else { "noncit" },
        )
    }
}
