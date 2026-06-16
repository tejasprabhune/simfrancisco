//! Filter a full-state ACS PUMS person file down to a city's PUMAs and write a small
//! committed subset so the server/validate need no network.
//!
//! Per-city:   cargo run --bin ingest_pums -- --city <slug> --input data/pums/psam_pXX.csv
//!             (output path + PUMA filter come from the city profile)
//! Legacy SF:  cargo run --bin ingest_pums -- [input_csv] [output_csv]
//!             (defaults: data/pums/psam_p06.csv -> data/sf_pums.csv)

use simfrancisco::city::CityProfile;
use simfrancisco::pums::{load_csv, write_subset, SF_PUMAS};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // --city <slug> --input <psam_pXX.csv>
    if let Some(i) = args.iter().position(|a| a == "--city") {
        let slug = args.get(i + 1).cloned().expect("--city needs a slug");
        let input = args
            .iter()
            .position(|a| a == "--input")
            .and_then(|j| args.get(j + 1))
            .cloned()
            .expect("--input <psam_pXX.csv> is required with --city");
        let profile = if slug == "sf" { CityProfile::sf() } else { CityProfile::load(&slug)? };
        eprintln!(
            "Loading {input}, filtering {} to {} PUMAs ...",
            profile.slug,
            profile.pumas.len()
        );
        let records = load_csv(&input, &profile.pumas)?;
        let total: f64 = records.iter().map(|r| r.pwgtp).sum();
        eprintln!("Loaded {} records; total PWGTP (≈ population) {:.0}", records.len(), total);
        write_subset(&records, &profile.pums_path)?;
        eprintln!("Wrote {} records to {}", records.len(), profile.pums_path);
        return Ok(());
    }

    // legacy positional SF path
    let input = args.first().cloned().unwrap_or_else(|| "data/pums/psam_p06.csv".to_string());
    let output = args.get(1).cloned().unwrap_or_else(|| "data/sf_pums.csv".to_string());
    eprintln!("Loading {input} and filtering to SF PUMAs {SF_PUMAS:?} ...");
    let records = load_csv(&input, &SF_PUMAS)?;
    eprintln!("Loaded {} SF person records.", records.len());
    let total_weight: f64 = records.iter().map(|r| r.pwgtp).sum();
    eprintln!("Total PWGTP (≈ SF population): {:.0}", total_weight);
    write_subset(&records, &output)?;
    eprintln!("Wrote {} records to {output}", records.len());
    Ok(())
}
