//! Filter the full California PUMS person file down to the 8 SF County PUMAs and write
//! a small committed subset (`data/sf_pums.csv`) so the server/validate need no network.
//!
//! Usage: cargo run --bin ingest_pums -- [input_csv] [output_csv]
//!   defaults: data/pums/psam_p06.csv -> data/sf_pums.csv

use simfrancisco::pums::{load_csv, write_subset, SF_PUMAS};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = args.next().unwrap_or_else(|| "data/pums/psam_p06.csv".to_string());
    let output = args.next().unwrap_or_else(|| "data/sf_pums.csv".to_string());

    eprintln!("Loading {input} and filtering to SF PUMAs {SF_PUMAS:?} ...");
    let records = load_csv(&input, &SF_PUMAS)?;
    eprintln!("Loaded {} SF person records.", records.len());

    let total_weight: f64 = records.iter().map(|r| r.pwgtp).sum();
    eprintln!("Total PWGTP (≈ SF population): {:.0}", total_weight);

    write_subset(&records, &output)?;
    eprintln!("Wrote {} records to {output}", records.len());
    Ok(())
}
