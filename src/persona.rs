//! Seeded, deterministic persona + value-vector generation, and the population builder.
//!
//! Persona generation is rule-based and seeded (no LLM call per agent) so runs are
//! reproducible and cheap. Seed = hash(simulation_seed, agent_index). The LLM is only
//! used later at poll/reaction time. Value vectors are calibrated to San Francisco's
//! electorate (strongly progressive on average, with real heterogeneity by age, race,
//! education, income, tenure, religiosity) — but the *specific* vote on a given measure
//! is left to the LLM reasoning over the persona, not hardcoded.

use crate::agent::{Agent, ValueVector};
use crate::geo::{Cell, TilesDb};
use crate::pums::PumsRecord;
use crate::religion;
use rand::Rng;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use sha2::{Digest, Sha256};

pub fn agent_seed(sim_seed: u64, idx: u32) -> u64 {
    let mut h = Sha256::new();
    h.update(sim_seed.to_le_bytes());
    h.update(idx.to_le_bytes());
    let d = h.finalize();
    u64::from_le_bytes(d[0..8].try_into().unwrap())
}

pub struct Population {
    pub agents: Vec<Agent>,
    pub income_cutoffs: [f64; 4],
    pub seed: u64,
    pub n: usize,
}

impl Population {
    pub fn total_weight(&self) -> f64 {
        self.agents.iter().map(|a| a.weight()).sum()
    }
}

/// Build a population of `n` agents by sampling PUMS records (uniformly, carrying
/// PWGTP). Deterministic given `seed`. If `tiles` is provided, assigns home/work cells.
pub fn build_population(
    records: &[PumsRecord],
    n: usize,
    seed: u64,
    tiles: Option<&TilesDb>,
) -> Population {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let with_replacement = n > records.len();

    // choose record indices
    let mut chosen: Vec<usize> = Vec::with_capacity(n);
    if with_replacement {
        for _ in 0..n {
            chosen.push(rng.gen_range(0..records.len()));
        }
    } else {
        // partial Fisher-Yates without replacement
        let mut pool: Vec<usize> = (0..records.len()).collect();
        for i in 0..n {
            let j = rng.gen_range(i..pool.len());
            pool.swap(i, j);
            chosen.push(pool[i]);
        }
    }

    // weighted income quintile cutoffs over the sampled population
    let mut inc_pairs: Vec<(f64, f64)> = chosen
        .iter()
        .map(|&i| (records[i].econ_rank(), records[i].pwgtp))
        .collect();
    let income_cutoffs = weighted_quintile_cutoffs(&mut inc_pairs);

    let mut agents = Vec::with_capacity(n);
    for (idx, &ri) in chosen.iter().enumerate() {
        let rec = records[ri].clone();
        let aseed = agent_seed(seed, idx as u32);
        let mut prng = ChaCha8Rng::seed_from_u64(aseed);
        let agent = make_agent(idx as u32, rec, aseed, &mut prng, tiles, &income_cutoffs);
        agents.push(agent);
    }
    Population {
        agents,
        income_cutoffs,
        seed,
        n,
    }
}

fn weighted_quintile_cutoffs(pairs: &mut [(f64, f64)]) -> [f64; 4] {
    pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let total: f64 = pairs.iter().map(|p| p.1).sum();
    if total <= 0.0 {
        return [0.0; 4];
    }
    let targets = [0.2, 0.4, 0.6, 0.8];
    let mut cutoffs = [0.0f64; 4];
    let mut cum = 0.0;
    let mut ti = 0;
    for &(inc, w) in pairs.iter() {
        cum += w;
        while ti < 4 && cum / total >= targets[ti] {
            cutoffs[ti] = inc;
            ti += 1;
        }
        if ti >= 4 {
            break;
        }
    }
    for i in 0..4 {
        if cutoffs[i] == 0.0 && i > 0 {
            cutoffs[i] = cutoffs[i - 1];
        }
    }
    cutoffs
}

fn make_agent(
    id: u32,
    rec: PumsRecord,
    seed: u64,
    rng: &mut impl Rng,
    tiles: Option<&TilesDb>,
    cutoffs: &[f64; 4],
) -> Agent {
    let religion = religion::assign(&rec, rng);
    let religiosity = religion::religiosity(religion, rng);
    let homeowner = sample_homeowner(&rec, rng);
    let q = cutoffs.iter().filter(|&&c| rec.econ_rank() > c).count();
    let values = make_value_vector(&rec, religiosity, homeowner, q, rng);
    let name = make_name(&rec, rng);
    let occupation = occupation_label(rec.occp, rec.esr);
    let neighborhood = neighborhood_label(rec.puma);
    let persona = build_persona_prose(&rec, &name, &occupation, &neighborhood, religion, religiosity, homeowner, &values);

    let (home, work) = if let Some(t) = tiles {
        let home = t.sample_residential_cell(rec.puma, rng);
        let work = if rec.employed() {
            Some(sample_work_cell(t, &rec, rng))
        } else {
            None
        };
        (home, work)
    } else {
        (Cell::new(0, 0), None)
    };

    Agent {
        id,
        rec,
        seed,
        name,
        religion,
        religiosity,
        homeowner,
        values,
        persona,
        occupation,
        neighborhood,
        home,
        work,
    }
}

/// SF tenure ≈ 38% owner / 62% renter. Probability of owning rises with income, age,
/// and marriage. Logistic-ish; deterministic given rng.
fn sample_homeowner(rec: &PumsRecord, rng: &mut impl Rng) -> bool {
    // econ_rank is POVPIP (0–501). Higher household economic standing → more likely owner.
    let pov = rec.econ_rank();
    let mut z = -1.4_f64;
    z += ((pov - 250.0) / 150.0).clamp(-1.6, 1.8) * 0.6;
    z += ((rec.age as f64 - 40.0) / 20.0) * 0.5;
    if rec.mar == 1 {
        z += 0.5;
    }
    if rec.foreign_born() {
        z -= 0.2;
    }
    let p = 1.0 / (1.0 + (-z).exp());
    rng.gen::<f64>() < p
}

/// SF-calibrated value vector with demographic deltas + small seeded noise.
pub fn make_value_vector(
    rec: &PumsRecord,
    religiosity: f64,
    homeowner: bool,
    income_q: usize,
    rng: &mut impl Rng,
) -> ValueVector {
    let age = rec.age as f64;
    let college = rec.college_plus();
    let race = rec.race_eth();

    // baselines: SF adult is economically-left, socially-progressive
    let mut economic = -0.33;
    let mut social = -0.45;
    let mut trust = -0.03;
    let mut change = 0.05;

    // economic axis
    economic += (income_q as f64 - 2.0) * 0.06;
    economic += (age - 45.0) / 100.0 * 0.5;
    economic += match race {
        "black" => -0.15,
        "hispanic" => -0.08,
        "asian" => 0.04,
        _ => 0.0,
    };
    if homeowner {
        economic += 0.08;
    }
    if college {
        economic -= 0.03;
    }

    // social axis
    if college {
        social -= 0.15;
    }
    social += (age - 45.0) / 100.0 * 0.6;
    social += religiosity * 0.4;
    if rec.foreign_born() {
        social += 0.1;
    }
    social += match race {
        "black" | "hispanic" => 0.05,
        "asian" => 0.03,
        _ => 0.0,
    };

    // trust
    if college {
        trust += 0.05;
    }
    trust += (income_q as f64 - 2.0) * 0.03;
    trust += (age - 45.0) / 100.0 * 0.2;

    // change vs status quo
    if !homeowner {
        change += 0.1;
    }
    if income_q <= 1 {
        change += 0.08;
    }
    change += (45.0 - age) / 100.0 * 0.3;

    // saliences (SF issues run hot)
    let mut s_housing = 0.6;
    let mut s_crime = 0.45;
    let s_homeless = 0.6;
    let mut s_cost = 0.65;
    let mut s_environment = 0.55;
    let mut s_immigration = 0.35;
    if !homeowner {
        s_housing += 0.2;
        s_cost += 0.15;
    } else {
        s_crime += 0.15;
    }
    if age > 50.0 {
        s_crime += 0.12;
    }
    if college {
        s_environment += 0.1;
    }
    if rec.foreign_born() {
        s_immigration += 0.25;
    }
    if income_q <= 1 {
        s_cost += 0.1;
    }

    let mut n = || rng.gen_range(-0.1f64..0.1);
    let mut v = ValueVector {
        economic: economic + n(),
        social: social + n(),
        trust: trust + n(),
        change: change + n(),
        s_housing: s_housing + n(),
        s_crime: s_crime + n(),
        s_homeless: s_homeless + n(),
        s_cost: s_cost + n(),
        s_environment: s_environment + n(),
        s_immigration: s_immigration + n(),
    };
    v.clamp();
    v
}

fn build_persona_prose(
    rec: &PumsRecord,
    name: &str,
    occupation: &str,
    neighborhood: &str,
    religion: crate::religion::Religion,
    religiosity: f64,
    homeowner: bool,
    values: &ValueVector,
) -> String {
    let tenure = if homeowner { "owns their home" } else { "rents" };
    let relig = if religiosity < 0.15 {
        "not religious".to_string()
    } else {
        format!("{} ({})", religion.label(), if religiosity > 0.55 { "observant" } else { "somewhat observant" })
    };
    let edu = match rec.educ() {
        "lt_hs" => "did not finish high school",
        "hs" => "high-school educated",
        "some_college" => "some college",
        "bachelors" => "a bachelor's degree",
        _ => "a graduate degree",
    };
    let marital = match rec.marital() {
        "married" => "married",
        "divorced" => "divorced",
        "widowed" => "widowed",
        "separated" => "separated",
        _ => "single",
    };
    let born = if rec.foreign_born() {
        ", an immigrant to the US"
    } else {
        ""
    };
    format!(
        "{name}, age {age}, is a {marital} {race} San Franciscan{born} living in {neighborhood}. \
Works as {occ}, has {edu}, {tenure}, {relig}. Politically: {leanings}",
        age = rec.age,
        race = pretty_race(rec.race_eth()),
        occ = occupation,
        leanings = values.describe(),
    )
}

fn pretty_race(r: &str) -> &'static str {
    match r {
        "white" => "white",
        "black" => "Black",
        "asian" => "Asian",
        "hispanic" => "Latino/Hispanic",
        "pacific" => "Pacific Islander",
        "native" => "Native American",
        _ => "multiracial",
    }
}

pub fn neighborhood_label(puma: u32) -> String {
    match puma {
        7507 => "Bayview / Hunters Point",
        7508 => "the Richmond / Presidio",
        7509 => "Chinatown / North Beach / Russian Hill",
        7510 => "SoMa / the Mission",
        7511 => "Bernal Heights / the central city",
        7512 => "the Sunset",
        7513 => "Ingleside / Oceanview",
        7514 => "the Marina / Western Addition",
        _ => "San Francisco",
    }
    .to_string()
}

/// Coarse occupation bucket from 2018 OCCP code ranges.
pub fn occupation_label(occp: u32, esr: u8) -> String {
    if esr == 3 {
        return "currently unemployed".to_string();
    }
    if matches!(esr, 6 | 0) || occp == 0 {
        return "not in the workforce".to_string();
    }
    let s = match occp {
        10..=960 => "a manager or business professional",
        1000..=1240 => "a software engineer / tech worker",
        1300..=1560 => "an engineer",
        1600..=1980 => "a scientist or analyst",
        2000..=2060 => "a social-services worker",
        2100..=2180 => "a lawyer or legal worker",
        2200..=2555 => "a teacher or educator",
        2600..=2920 => "an artist, designer, or media worker",
        3000..=3550 => "a healthcare professional",
        3600..=4655 => "a service worker",
        4700..=5940 => "a sales or office worker",
        6000..=7630 => "a construction or trades worker",
        7700..=9760 => "a production or transportation worker",
        9800..=9830 => "in the military",
        _ => "a worker",
    };
    s.to_string()
}

fn sample_work_cell(tiles: &TilesDb, rec: &PumsRecord, rng: &mut impl Rng) -> Cell {
    // Most SF jobs concentrate downtown (SoMa/FiDi); some stay near home PUMA.
    if rng.gen::<f64>() < 0.55 {
        // downtown cluster ~ chunk (44, 24)
        tiles.sample_residential_cell(7510, rng)
    } else {
        tiles.sample_residential_cell(rec.puma, rng)
    }
}

fn make_name(rec: &PumsRecord, rng: &mut impl Rng) -> String {
    let first = if rec.sex == 1 {
        FIRST_M[rng.gen_range(0..FIRST_M.len())]
    } else {
        FIRST_F[rng.gen_range(0..FIRST_F.len())]
    };
    let last = match rec.race_eth() {
        "hispanic" => LAST_HISP[rng.gen_range(0..LAST_HISP.len())],
        "asian" => LAST_ASIAN[rng.gen_range(0..LAST_ASIAN.len())],
        "black" => LAST_BLACK[rng.gen_range(0..LAST_BLACK.len())],
        _ => LAST_GEN[rng.gen_range(0..LAST_GEN.len())],
    };
    format!("{first} {last}")
}

const FIRST_M: [&str; 12] = ["James", "Wei", "Carlos", "David", "Miguel", "Jamal", "Kevin", "Daniel", "Hassan", "Raj", "Tomás", "Andre"];
const FIRST_F: [&str; 12] = ["Maria", "Mei", "Sofia", "Aisha", "Jennifer", "Priya", "Keisha", "Elena", "Grace", "Fatima", "Lucia", "Nora"];
const LAST_GEN: [&str; 10] = ["Smith", "Johnson", "Miller", "O'Brien", "Goldberg", "Anderson", "Murphy", "Clark", "Reed", "Walsh"];
const LAST_HISP: [&str; 8] = ["Garcia", "Hernandez", "Lopez", "Gonzalez", "Rodriguez", "Ramirez", "Flores", "Cruz"];
const LAST_ASIAN: [&str; 8] = ["Chen", "Wong", "Nguyen", "Kim", "Lee", "Patel", "Tanaka", "Singh"];
const LAST_BLACK: [&str; 6] = ["Washington", "Jefferson", "Brooks", "Coleman", "Banks", "Carter"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pums::{PumsRecord, SF_PUMAS};

    fn rec(age: u8, hisp: u16, rac1p: u8, schl: u8, povpip: f64, cit: u8) -> PumsRecord {
        PumsRecord {
            serialno: "x".into(), sporder: 1, pwgtp: 12.0, age, sex: if age % 2 == 0 { 1 } else { 2 },
            rac1p, hisp, schl, pincp: povpip * 0.6, povpip, occp: 1020, cow: 1, esr: 1, cit, mar: 5,
            nativity: 1, puma: 7510, adjinc: 1.0,
        }
    }

    #[test]
    fn deterministic_population() {
        let recs: Vec<PumsRecord> = (0..500)
            .map(|i| rec(20 + (i % 60) as u8, if i % 4 == 0 { 2 } else { 1 }, 1 + (i % 6) as u8, 16 + (i % 8) as u8, 40000.0 + (i as f64) * 500.0, 1))
            .collect();
        let p1 = build_population(&recs, 200, 42, None);
        let p2 = build_population(&recs, 200, 42, None);
        assert_eq!(p1.agents.len(), 200);
        // same seed -> identical agents (value vectors + names)
        for (a, b) in p1.agents.iter().zip(p2.agents.iter()) {
            assert_eq!(a.name, b.name);
            assert_eq!(a.values, b.values);
            assert_eq!(a.religion, b.religion);
        }
        // different seed -> different draw
        let p3 = build_population(&recs, 200, 7, None);
        let same = p1.agents.iter().zip(p3.agents.iter()).filter(|(a, b)| a.rec.serialno == b.rec.serialno && a.name == b.name).count();
        assert!(same < 200);
    }

    #[test]
    fn sf_is_progressive_on_average() {
        let recs: Vec<PumsRecord> = (0..1000)
            .map(|i| rec(18 + (i % 70) as u8, if i % 5 == 0 { 2 } else { 1 }, 1 + (i % 6) as u8, 16 + (i % 9) as u8, 30000.0 + (i as f64) * 300.0, 1))
            .collect();
        let p = build_population(&recs, 800, 42, None);
        let mean_social: f64 = p.agents.iter().map(|a| a.values.social).sum::<f64>() / 800.0;
        let mean_econ: f64 = p.agents.iter().map(|a| a.values.economic).sum::<f64>() / 800.0;
        assert!(mean_social < -0.2, "mean social {mean_social} should be progressive");
        assert!(mean_econ < -0.1, "mean econ {mean_econ} should be left");
    }

    #[test]
    fn marginals_match_population() {
        // "marginals-match": sampling uniformly while carrying PWGTP must reproduce the
        // weighted marginal distribution of the source records within tolerance. This is the
        // core guarantee of sampling from PUMS (weighted sample marginal == ACS marginal).
        use std::collections::HashMap;
        let mut recs = Vec::new();
        for i in 0..3000u32 {
            // varied demographics + heterogeneous weights
            let age = 18 + (i % 70) as u8;
            let rac1p = 1 + (i % 6) as u8;
            let hisp = if i % 5 == 0 { 2 } else { 1 };
            let schl = 10 + (i % 15) as u8;
            let w = 5.0 + (i % 30) as f64; // unequal weights
            recs.push(PumsRecord {
                serialno: format!("r{i}"), sporder: 1, pwgtp: w, age, sex: 1 + (i % 2) as u8,
                rac1p, hisp, schl, pincp: 30000.0, povpip: 50.0 + (i % 400) as f64, occp: 1020,
                cow: 1, esr: 1, cit: if i % 9 == 0 { 5 } else { 1 }, mar: 5, nativity: 1,
                puma: SF_PUMAS[(i % 8) as usize], adjinc: 1.0,
            });
        }
        let weighted_marg = |race_fn: &dyn Fn(&PumsRecord) -> String, items: &[PumsRecord], wt: &dyn Fn(&PumsRecord) -> f64| -> HashMap<String, f64> {
            let mut m: HashMap<String, f64> = HashMap::new();
            let mut tot = 0.0;
            for r in items { let w = wt(r); *m.entry(race_fn(r)).or_default() += w; tot += w; }
            for v in m.values_mut() { *v /= tot.max(1e-9); }
            m
        };
        let pop = build_population(&recs, 1500, 99, None);
        // compare race_eth marginal: full records vs sampled agents, both PWGTP-weighted
        let target = weighted_marg(&|r| r.race_eth().to_string(), &recs, &|r| r.pwgtp);
        let agent_recs: Vec<PumsRecord> = pop.agents.iter().map(|a| a.rec.clone()).collect();
        let emp = weighted_marg(&|r| r.race_eth().to_string(), &agent_recs, &|r| r.pwgtp);
        let mut keys: std::collections::HashSet<&String> = target.keys().collect();
        keys.extend(emp.keys());
        let mut tv = 0.0;
        for k in keys { tv += (target.get(k).copied().unwrap_or(0.0) - emp.get(k).copied().unwrap_or(0.0)).abs(); }
        tv /= 2.0;
        assert!(tv < 0.05, "race_eth marginal TV distance {tv} exceeds tolerance");
    }

    #[test]
    fn income_quintiles_monotonic() {
        let recs: Vec<PumsRecord> = (0..1000)
            .map(|i| rec(40, 1, 1, 20, 10000.0 + (i as f64) * 200.0, 1))
            .collect();
        let p = build_population(&recs, 1000, 1, None);
        assert!(p.income_cutoffs[0] <= p.income_cutoffs[1]);
        assert!(p.income_cutoffs[1] <= p.income_cutoffs[2]);
        assert!(p.income_cutoffs[2] <= p.income_cutoffs[3]);
    }
}
