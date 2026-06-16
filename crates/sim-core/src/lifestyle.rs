//! Richer, non-political persona layer: hobbies, a daily routine, and a spending
//! tilt, derived DETERMINISTICALLY from an agent's demographics + real public-survey
//! tables (BLS ATUS time-use, BLS Consumer Expenditure Survey). No LLM, no global RNG —
//! every draw uses the agent's seeded prng, so populations stay byte-reproducible.
//!
//! Tables are embedded at compile time so this works identically in tests and at runtime.

use crate::pums::PumsRecord;
use once_cell::sync::Lazy;
use rand::Rng;

const ATUS_CSV: &str = include_str!("../../../data/survey/atus_routine.csv");
const HOBBIES_CSV: &str = include_str!("../../../data/survey/hobbies.csv");
const CEX_CSV: &str = include_str!("../../../data/survey/cex_spending.csv");

#[derive(Clone, Debug)]
pub struct Lifestyle {
    pub hobbies: Vec<String>,
    pub routine: String,
    pub spending: String,
}

struct RoutineRow {
    age_band: String,
    employed: bool,
    wake: f64,
    work: f64,
    leisure: f64,
    sleep: f64,
}

struct HobbyRow {
    hobby: String,
    age_grp: String,
    sex: String,
    weight: f64,
}

static ROUTINE: Lazy<Vec<RoutineRow>> = Lazy::new(|| {
    ATUS_CSV
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("age_band") && !l.trim().is_empty())
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            if f.len() < 9 {
                return None;
            }
            Some(RoutineRow {
                age_band: f[0].to_string(),
                employed: f[1] == "yes",
                wake: f[2].parse().ok()?,
                work: f[3].parse().ok()?,
                leisure: f[4].parse().ok()?,
                sleep: f[8].parse().ok()?,
            })
        })
        .collect()
});

static HOBBIES: Lazy<Vec<HobbyRow>> = Lazy::new(|| {
    HOBBIES_CSV
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("hobby") && !l.trim().is_empty())
        .filter_map(|l| {
            let f: Vec<&str> = l.split(',').collect();
            if f.len() < 4 {
                return None;
            }
            Some(HobbyRow {
                hobby: f[0].to_string(),
                age_grp: f[1].to_string(),
                sex: f[2].to_string(),
                weight: f[3].parse().ok()?,
            })
        })
        .collect()
});

// income_quintile (1-5) -> (dining_out, entertainment, apparel) discretionary share sum.
static CEX_DISCRETIONARY: Lazy<[f64; 5]> = Lazy::new(|| {
    let mut out = [0.0f64; 5];
    for l in CEX_CSV.lines() {
        if l.starts_with('#') || l.starts_with("income_quintile") || l.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = l.split(',').collect();
        if f.len() < 3 {
            continue;
        }
        let (q, cat, share) = (f[0].parse::<usize>(), f[1], f[2].parse::<f64>());
        if let (Ok(q), Ok(share)) = (q, share) {
            if (1..=5).contains(&q) && matches!(cat, "dining_out" | "entertainment" | "apparel") {
                out[q - 1] += share;
            }
        }
    }
    out
});

fn age_group(age: u8) -> &'static str {
    if age < 35 {
        "young"
    } else if age < 55 {
        "mid"
    } else {
        "older"
    }
}

fn hobby_label(key: &str) -> &'static str {
    match key {
        "hiking_outdoors" => "hiking and the outdoors",
        "team_sports" => "team sports",
        "gym_fitness" => "the gym",
        "gaming" => "video games",
        "reading" => "reading",
        "cooking" => "cooking",
        "dining_out" => "dining out",
        "live_music_arts" => "live music and the arts",
        "gardening" => "gardening",
        "volunteering" => "volunteering",
        "religious_activities" => "religious activities",
        "travel" => "travel",
        _ => "their hobbies",
    }
}

/// Deterministic lifestyle for an agent. `income_q` is the 0-4 income quintile index.
pub fn generate(rec: &PumsRecord, income_q: usize, rng: &mut impl Rng) -> Lifestyle {
    let age_grp = age_group(rec.age);
    let sex = if rec.sex == 1 { "M" } else { "F" };

    // Weighted sample of 2-3 hobbies for this demographic (without replacement).
    let mut pool: Vec<(&str, f64)> = HOBBIES
        .iter()
        .filter(|h| h.age_grp == age_grp && h.sex == sex)
        .map(|h| (h.hobby.as_str(), h.weight))
        .collect();
    let mut hobbies = Vec::new();
    let n_pick = 2 + (rng.gen::<f64>() < 0.5) as usize; // 2 or 3
    for _ in 0..n_pick {
        let total: f64 = pool.iter().map(|(_, w)| *w).sum();
        if total <= 0.0 || pool.is_empty() {
            break;
        }
        let mut t = rng.gen::<f64>() * total;
        let mut chosen = 0usize;
        for (i, (_, w)) in pool.iter().enumerate() {
            t -= *w;
            if t <= 0.0 {
                chosen = i;
                break;
            }
        }
        hobbies.push(hobby_label(pool[chosen].0).to_string());
        pool.remove(chosen);
    }

    // Daily routine from ATUS (by age band + employment).
    let employed = rec.employed();
    let age_band = rec.age_band().to_string();
    let routine = ROUTINE
        .iter()
        .find(|r| r.age_band == age_band && r.employed == employed)
        .map(|r| {
            let wake = fmt_hour(r.wake);
            let sleep = fmt_hour(if r.sleep >= 24.0 { r.sleep - 24.0 } else { r.sleep });
            if r.work >= 4.0 {
                format!(
                    "wakes around {wake}, works about {:.0}h, has roughly {:.0}h of leisure, and sleeps near {sleep}",
                    r.work, r.leisure
                )
            } else {
                format!(
                    "wakes around {wake}, isn't working full-time, has roughly {:.0}h of leisure, and sleeps near {sleep}",
                    r.leisure
                )
            }
        })
        .unwrap_or_default();

    // Spending tilt from CEX (discretionary share by income quintile).
    let disc = CEX_DISCRETIONARY[income_q.min(4)];
    let spending = if disc >= 0.16 {
        "spends a relatively large share on dining out, entertainment, and apparel".to_string()
    } else if disc <= 0.115 {
        "spends most of their budget on housing, food at home, and necessities".to_string()
    } else {
        "has a typical mix of necessary and discretionary spending".to_string()
    };

    Lifestyle { hobbies, routine, spending }
}

fn fmt_hour(h: f64) -> String {
    let hr = h.floor() as i32;
    let min = ((h - hr as f64) * 60.0).round() as i32;
    let (h12, ampm) = if hr == 0 {
        (12, "am")
    } else if hr < 12 {
        (hr, "am")
    } else if hr == 12 {
        (12, "pm")
    } else {
        (hr - 12, "pm")
    };
    if min == 0 {
        format!("{h12}{ampm}")
    } else {
        format!("{h12}:{min:02}{ampm}")
    }
}
