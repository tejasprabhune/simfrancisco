//! ACS PUMS person-microdata ingest for San Francisco County.
//!
//! We sample agents from real joint microdata so the joint distribution over age,
//! sex, race/ethnicity, education, income, occupation, citizenship, marital status
//! comes for free. Every record carries the PUMS person weight `PWGTP`, which all
//! population estimates use.
//!
//! Source: Census ACS 1-Year PUMS flat file (csv_pca.zip → psam_p06.csv), filtered
//! to the 8 SF County PUMAs (07507–07514). The `ingest_pums` binary writes the
//! filtered SF subset to `data/sf_pums.csv` (committed) so the server/validate are
//! self-contained and need no network at runtime.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, BufReader};

/// The 8 San Francisco County 2020-vintage PUMAs (verified vs the Census PUMA names file).
pub const SF_PUMAS: [u32; 8] = [7507, 7508, 7509, 7510, 7511, 7512, 7513, 7514];

/// Columns we keep from PUMS (header-indexed, so column order is irrelevant).
/// The person flat file has no HINCP; POVPIP (household income-to-poverty ratio,
/// 0–501) is the per-person household economic-standing measure we use for income rank.
pub const KEEP_COLS: [&str; 18] = [
    "SERIALNO", "SPORDER", "PWGTP", "AGEP", "SEX", "RAC1P", "HISP", "SCHL", "PINCP", "POVPIP",
    "OCCP", "COW", "ESR", "CIT", "MAR", "NATIVITY", "PUMA", "ADJINC",
];

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PumsRecord {
    pub serialno: String,
    pub sporder: u32,
    pub pwgtp: f64,
    pub age: u8,
    pub sex: u8,
    pub rac1p: u8,
    pub hisp: u16,
    pub schl: u8,
    pub pincp: f64,
    pub povpip: f64,
    pub occp: u32,
    pub cow: u8,
    pub esr: u8,
    pub cit: u8,
    pub mar: u8,
    pub nativity: u8,
    pub puma: u32,
    pub adjinc: f64,
}

impl PumsRecord {
    /// Household economic standing: income-to-poverty ratio (0–501). N/A clamps to 0.
    /// Used as the income-rank scalar for quintiles (no household-file join needed).
    pub fn econ_rank(&self) -> f64 {
        if self.povpip < 0.0 {
            0.0
        } else {
            self.povpip
        }
    }
    pub fn person_income(&self) -> f64 {
        if self.pincp <= -19999.0 {
            0.0
        } else {
            (self.pincp * self.adjinc).max(0.0)
        }
    }
    pub fn is_citizen(&self) -> bool {
        (1..=4).contains(&self.cit)
    }
    /// Citizen voting-age population eligibility.
    pub fn is_cvap(&self) -> bool {
        self.is_citizen() && self.age >= 18
    }
    pub fn age_band(&self) -> &'static str {
        match self.age {
            0..=17 => "u18",
            18..=24 => "18-24",
            25..=34 => "25-34",
            35..=44 => "35-44",
            45..=54 => "45-54",
            55..=64 => "55-64",
            _ => "65+",
        }
    }
    /// Collapsed race/ethnicity (Hispanic takes precedence over race code).
    pub fn race_eth(&self) -> &'static str {
        if self.hisp != 1 {
            return "hispanic";
        }
        match self.rac1p {
            1 => "white",
            2 => "black",
            6 => "asian",
            7 => "pacific",
            3 | 4 | 5 => "native",
            _ => "other_multi",
        }
    }
    /// Education collapsed to 4 levels (SCHL ranges).
    pub fn educ(&self) -> &'static str {
        match self.schl {
            0..=15 => "lt_hs",
            16..=17 => "hs",
            18..=20 => "some_college",
            21 => "bachelors",
            _ => "graduate",
        }
    }
    pub fn college_plus(&self) -> bool {
        self.schl >= 21
    }
    pub fn marital(&self) -> &'static str {
        match self.mar {
            1 => "married",
            2 => "widowed",
            3 => "divorced",
            4 => "separated",
            _ => "never_married",
        }
    }
    pub fn employed(&self) -> bool {
        matches!(self.esr, 1 | 2 | 4 | 5)
    }
    pub fn foreign_born(&self) -> bool {
        self.nativity == 2
    }
}

fn parse_csv_line(line: &str) -> Vec<String> {
    // PUMS flat files quote only SERIALNO/RT and never embed commas in a field,
    // so a comma split + quote-strip is correct and avoids a CSV dependency.
    line.split(',')
        .map(|f| f.trim().trim_matches('"').to_string())
        .collect()
}

/// Load PUMS person records from a CSV (full CA file or the SF subset), keeping only
/// rows whose PUMA is in `pumas`. Header-indexed; tolerates `ST` or `STATE`.
pub fn load_csv(path: &str, pumas: &[u32]) -> Result<Vec<PumsRecord>> {
    let f = std::fs::File::open(path).with_context(|| format!("open pums csv {path}"))?;
    let mut reader = BufReader::new(f);
    let mut header = String::new();
    reader.read_line(&mut header)?;
    let cols = parse_csv_line(&header);
    let idx: HashMap<String, usize> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| (c.to_ascii_uppercase(), i))
        .collect();
    let need = |name: &str| -> Result<usize> {
        idx.get(name)
            .copied()
            .ok_or_else(|| anyhow!("PUMS csv missing column {name}"))
    };
    let i_serial = need("SERIALNO")?;
    let i_sporder = need("SPORDER")?;
    let i_pwgtp = need("PWGTP")?;
    let i_age = need("AGEP")?;
    let i_sex = need("SEX")?;
    let i_rac1p = need("RAC1P")?;
    let i_hisp = need("HISP")?;
    let i_schl = need("SCHL")?;
    let i_pincp = need("PINCP")?;
    let i_povpip = need("POVPIP")?;
    let i_occp = need("OCCP")?;
    let i_cow = need("COW")?;
    let i_esr = need("ESR")?;
    let i_cit = need("CIT")?;
    let i_mar = need("MAR")?;
    let i_nativity = need("NATIVITY")?;
    let i_puma = need("PUMA")?;
    let i_adjinc = need("ADJINC")?;

    let pumaset: std::collections::HashSet<u32> = pumas.iter().copied().collect();
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let v = parse_csv_line(&line);
        if v.len() <= i_adjinc {
            continue;
        }
        let puma: u32 = v[i_puma].trim_start_matches('0').parse().unwrap_or(0);
        if !pumaset.is_empty() && !pumaset.contains(&puma) {
            continue;
        }
        let gf = |i: usize| -> f64 { v[i].parse().unwrap_or(0.0) };
        let gu = |i: usize| -> u64 { v[i].trim_start_matches('0').parse().unwrap_or(0) };
        let pwgtp = gf(i_pwgtp);
        if pwgtp <= 0.0 {
            continue;
        }
        out.push(PumsRecord {
            serialno: v[i_serial].clone(),
            sporder: gu(i_sporder) as u32,
            pwgtp,
            age: gu(i_age) as u8,
            sex: gu(i_sex) as u8,
            rac1p: gu(i_rac1p) as u8,
            hisp: gu(i_hisp) as u16,
            schl: gu(i_schl) as u8,
            pincp: gf(i_pincp),
            povpip: gf(i_povpip),
            occp: gu(i_occp) as u32,
            cow: gu(i_cow) as u8,
            esr: gu(i_esr) as u8,
            cit: gu(i_cit) as u8,
            mar: gu(i_mar) as u8,
            nativity: gu(i_nativity) as u8,
            puma,
            adjinc: gf(i_adjinc) / 1_000_000.0,
        });
    }
    if out.is_empty() {
        return Err(anyhow!("no PUMS records loaded from {path} for pumas {pumas:?}"));
    }
    Ok(out)
}

/// Write a filtered SF subset CSV with just the KEEP_COLS header (small, committable).
pub fn write_subset(records: &[PumsRecord], path: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = std::fs::File::create(path)?;
    writeln!(f, "{}", KEEP_COLS.join(","))?;
    for r in records {
        // ADJINC written back as the 6-implied-decimal integer for round-trip fidelity.
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:05},{}",
            r.serialno,
            r.sporder,
            r.pwgtp as i64,
            r.age,
            r.sex,
            r.rac1p,
            r.hisp,
            r.schl,
            r.pincp as i64,
            r.povpip as i64,
            r.occp,
            r.cow,
            r.esr,
            r.cit,
            r.mar,
            r.nativity,
            r.puma,
            (r.adjinc * 1_000_000.0).round() as i64
        )?;
    }
    Ok(())
}

pub fn default_sf_path() -> String {
    std::env::var("SF_PUMS_PATH").unwrap_or_else(|_| "data/sf_pums.csv".to_string())
}

/// Load the committed SF subset.
pub fn load_sf() -> Result<Vec<PumsRecord>> {
    load_csv(&default_sf_path(), &SF_PUMAS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(age: u8, hisp: u16, rac1p: u8, schl: u8, cit: u8) -> PumsRecord {
        PumsRecord {
            serialno: "x".into(),
            sporder: 1,
            pwgtp: 10.0,
            age,
            sex: 1,
            rac1p,
            hisp,
            schl,
            pincp: 50000.0,
            povpip: 350.0,
            occp: 0,
            cow: 1,
            esr: 1,
            cit,
            mar: 5,
            nativity: 1,
            puma: 7510,
            adjinc: 1.01,
        }
    }

    #[test]
    fn derived_categories() {
        let r = rec(30, 1, 6, 22, 1);
        assert_eq!(r.age_band(), "25-34");
        assert_eq!(r.race_eth(), "asian");
        assert_eq!(r.educ(), "graduate");
        assert!(r.college_plus());
        assert!(r.is_cvap());
        let h = rec(40, 2, 1, 16, 5);
        assert_eq!(h.race_eth(), "hispanic"); // hisp != 1 overrides race
        assert_eq!(h.educ(), "hs");
        assert!(!h.is_citizen()); // CIT 5
        assert!(!h.is_cvap());
    }

    #[test]
    fn income_adjustment_applied() {
        let r = rec(30, 1, 1, 21, 1);
        // econ_rank is POVPIP (no dollar adjustment); person_income applies ADJINC.
        assert!((r.econ_rank() - 350.0).abs() < 1e-6);
        assert!((r.person_income() - 50000.0 * 1.01).abs() < 1.0);
    }

    #[test]
    fn parse_line_strips_quotes() {
        let v = parse_csv_line("\"2023GQ001\",1, 21 ,06");
        assert_eq!(v[0], "2023GQ001");
        assert_eq!(v[2], "21");
    }
}
