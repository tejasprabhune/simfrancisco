//! Per-city configuration. Everything the synthetic-population engine needs that is
//! specific to a city (and cannot be inferred from PUMS microdata) lives here, loaded
//! from `data/cities/<slug>.toml`. SF is also available hardcoded via [`CityProfile::sf`]
//! so tests and offline fallbacks need no file IO.
//!
//! Adding a city = drop a `data/cities/<slug>.toml` + its PUMS subset + tiles.db. No code.

use serde::Deserialize;
use std::path::Path;

#[derive(Clone, Debug, Deserialize)]
pub struct CityProfile {
    /// Internal id, e.g. "sf", "neu_york".
    pub slug: String,
    /// Playful UI name, e.g. "sim francisco", "neu york".
    pub display: String,
    /// Real-world city name used in LLM prompts, e.g. "San Francisco", "New York City".
    pub prompt_name: String,
    /// Resident demonym used in persona prose, e.g. "San Franciscan".
    pub demonym: String,
    /// County/core PUMA codes (2020 vintage) covered by this city.
    pub pumas: Vec<u32>,
    /// Path to the committed PUMS subset CSV for this city.
    pub pums_path: String,
    /// Path to this city's tiles.db.
    pub tiles_path: String,
    /// PUMA -> neighborhood label.
    #[serde(default)]
    pub neighborhoods: Vec<NeighborhoodEntry>,
    /// PUMA -> approximate (chunk_cx, chunk_cy, radius_chunks) on this city's grid.
    #[serde(default)]
    pub centroids: Vec<CentroidEntry>,
    /// Pew metro religion shares, in [`crate::religion::Religion::all`] order:
    /// [Catholic, Protestant, OtherChristian, Unaffiliated, Jewish, Muslim, Buddhist, Hindu, Other].
    pub religion_weights: [f64; 9],
    pub politics: PoliticsProfile,
    pub work: WorkClustering,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NeighborhoodEntry {
    pub puma: u32,
    pub label: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CentroidEntry {
    pub puma: u32,
    pub cx: i64,
    pub cy: i64,
    pub radius: i64,
}

/// City-level electorate calibration + the LLM-prompt electorate description.
#[derive(Clone, Debug, Deserialize)]
pub struct PoliticsProfile {
    pub economic_base: f64,
    pub social_base: f64,
    pub trust_base: f64,
    pub change_base: f64,
    pub s_housing: f64,
    pub s_crime: f64,
    pub s_homeless: f64,
    pub s_cost: f64,
    pub s_environment: f64,
    pub s_immigration: f64,
    /// City-specific electorate paragraph injected into the Vote system prompt.
    /// Must use ONLY pre-cutoff priors (no post-as-of-date outcomes).
    pub vote_facts: String,
    /// City/state-specific mechanic injected into the Belief (market) system prompt.
    pub belief_facts: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WorkClustering {
    /// PUMA of the central business district where jobs concentrate.
    pub downtown_puma: u32,
    /// Fraction of employed agents whose work cell is drawn from the CBD.
    pub downtown_share: f64,
}

impl CityProfile {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading city profile {}: {e}", path.display()))?;
        let p: CityProfile = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parsing city profile {}: {e}", path.display()))?;
        Ok(p)
    }

    /// Load `data/cities/<slug>.toml` (relative to the workspace root / CWD).
    pub fn load(slug: &str) -> anyhow::Result<Self> {
        if slug == "sf" {
            // SF is the hardcoded source of truth (and the committed tiles.db lives at the
            // repo root, not under artifacts/). Prefer it even if a file is also present.
            return Ok(Self::sf());
        }
        Self::from_file(Path::new(&format!("data/cities/{slug}.toml")))
    }

    pub fn neighborhood(&self, puma: u32) -> String {
        self.neighborhoods
            .iter()
            .find(|n| n.puma == puma)
            .map(|n| n.label.clone())
            .unwrap_or_else(|| self.prompt_name.clone())
    }

    /// (chunk_cx, chunk_cy, radius_chunks) for a PUMA; falls back to grid centre.
    pub fn centroid(&self, puma: u32) -> (i64, i64, i64) {
        self.centroids
            .iter()
            .find(|c| c.puma == puma)
            .map(|c| (c.cx, c.cy, c.radius))
            .unwrap_or((33, 30, 12))
    }

    /// Vote-framing system prompt. The SF assembly is byte-identical to the original
    /// hand-tuned prompt (prompt_name="San Francisco" + the SF vote_facts).
    pub fn vote_prompt(&self) -> String {
        format!(
            "You simulate the {name} electorate for a nonpartisan academic forecasting model. \
Reason as a real {name} resident with the given profile and lived experience of the city on the given date. \
{facts} \
Weigh each measure on its merits as this resident actually would, given the city's real mood at the date — do NOT fall back on ideological stereotypes. \
Use ONLY knowledge available on the given date; never use any outcome that occurred after it. \
For each profile, estimate the probability THIS resident casts a YES vote. Respond with STRICT JSON only, no prose outside it.",
            name = self.prompt_name,
            facts = self.politics.vote_facts,
        )
    }

    /// Belief-framing (prediction-market) system prompt.
    pub fn belief_prompt(&self) -> String {
        format!(
            "You are a calibrated political forecaster simulating informed {name} residents for a prediction-market model. \
For each resident profile, output a well-calibrated probability for the described event, reasoning ANALYTICALLY about the political landscape and election mechanics — not partisan hope. \
{facts} \
Estimate what is actually most likely to happen, not what a partisan would prefer. \
Use ONLY knowledge available on the given date; never use any outcome that occurred after it. Respond with STRICT JSON only, no prose outside it.",
            name = self.prompt_name,
            facts = self.politics.belief_facts,
        )
    }

    /// Non-political / multi-option (lifestyle, preference, multi-candidate) system prompt.
    /// Reasons from the full persona rather than the electorate framing.
    pub fn options_prompt(&self) -> String {
        format!(
            "You simulate individual residents of {name} for a population-level preference model. \
Reason as a real {name} resident with the given profile — their age, income, occupation, education, household, neighborhood, lifestyle, and personal values and tastes. \
You will be given a question and a list of labelled options. For each resident, output how likely THIS specific person is to choose each option, as a probability distribution over the options that sums to 1. \
Ground every answer in who this person actually is, not demographic stereotypes; people are heterogeneous. \
Use ONLY knowledge available on the given date. Respond with STRICT JSON only, no prose outside it.",
            name = self.prompt_name,
        )
    }

    /// The hardcoded San Francisco profile (single source of truth for SF). Matches the
    /// original per-module SF constants exactly.
    pub fn sf() -> Self {
        CityProfile {
            slug: "sf".into(),
            display: "sim francisco".into(),
            prompt_name: "San Francisco".into(),
            demonym: "San Franciscan".into(),
            pumas: vec![7507, 7508, 7509, 7510, 7511, 7512, 7513, 7514],
            pums_path: crate::pums::default_sf_path(),
            tiles_path: std::env::var("TILES_DB").unwrap_or_else(|_| "tiles.db".into()),
            neighborhoods: [
                (7507, "Bayview / Hunters Point"),
                (7508, "the Richmond / Presidio"),
                (7509, "Chinatown / North Beach / Russian Hill"),
                (7510, "SoMa / the Mission"),
                (7511, "Bernal Heights / the central city"),
                (7512, "the Sunset"),
                (7513, "Ingleside / Oceanview"),
                (7514, "the Marina / Western Addition"),
            ]
            .iter()
            .map(|(p, l)| NeighborhoodEntry { puma: *p, label: l.to_string() })
            .collect(),
            centroids: [
                (7507, 48, 44, 8),
                (7508, 14, 18, 8),
                (7509, 44, 14, 6),
                (7510, 40, 28, 7),
                (7511, 34, 38, 7),
                (7512, 16, 38, 9),
                (7513, 28, 48, 8),
                (7514, 30, 15, 6),
            ]
            .iter()
            .map(|(p, cx, cy, r)| CentroidEntry { puma: *p, cx: *cx, cy: *cy, radius: *r })
            .collect(),
            religion_weights: [0.24, 0.19, 0.02, 0.42, 0.02, 0.01, 0.03, 0.04, 0.03],
            politics: PoliticsProfile {
                economic_base: -0.33,
                social_base: -0.45,
                trust_base: -0.03,
                change_base: 0.05,
                s_housing: 0.6,
                s_crime: 0.45,
                s_homeless: 0.6,
                s_cost: 0.65,
                s_environment: 0.55,
                s_immigration: 0.35,
                vote_facts: "San Francisco is one of the most Democratic-leaning cities in the United States — strong partisans here vote their lean about 85–95% of the time, so be realistic and confident, not hedged, when a profile clearly leans one way. At the TOP of the ticket SF is especially lopsided: in the most recent prior presidential elections the Republican nominee won only about one in ten San Francisco voters citywide (roughly 9% in 2016 and 13% in 2020), and even higher-income, older, and homeowner residents vote Democratic for president at high rates. Residents split far more on local and state ballot MEASURES, where they weigh each proposition on its own merits. But residents are pragmatic and not monolithic: by 2022 voters had recalled progressive District Attorney Chesa Boudin (June 2022) and three school-board members amid frustration over public safety, retail theft, open-air drug markets, and city governance.".into(),
                belief_facts: "Key mechanic: California uses a TOP-TWO primary — the two highest vote-getters advance to the general election regardless of party, so when one prominent candidate is the only major contender from the minority party, that candidate usually consolidates enough of the minority vote to take second place even in a heavily one-party state.".into(),
            },
            work: WorkClustering { downtown_puma: 7510, downtown_share: 0.55 },
        }
    }
}
