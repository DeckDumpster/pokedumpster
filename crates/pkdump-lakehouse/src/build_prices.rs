//! `pkdump-lake-build-prices` — `catalog.prices` and `catalog.sealed_prices` for one date, from
//! the raw landing zone. Nothing else, and it never calls an upstream.
//!
//! The Rust replacement for `lake/src/pkdump_lake/prices.py` (sp-4k7mxi.4), on
//! `dumpster-kit-iceberg`. The reduction from landed bytes to rows is [`prices_core`], held to
//! the Python's recorded output by `tests/prices_parity.rs`; both tables come from ONE read of
//! the partition and carry the same `pkdump.raw-runs`, so they cannot be different nights.
//!
//! ## Replacing a day
//!
//! A date that already holds rows is replaced in one Iceberg `overwrite` commit per table, as
//! the Python job does (see [`replace`]). `--table` / `--sealed-table` are gone: the kit names
//! exactly these two tables.

#[path = "prices_core.rs"]
mod prices_core;
mod replace;

use anyhow::{Context, bail};
use clap::Parser;
use dumpster_kit_iceberg::{LakeCatalogConfig, prices};
use dumpster_kit_lake::{PartRecord, RawZone};

#[derive(Parser)]
#[command(name = "pkdump-lake-build-prices", version, about)]
struct Cli {
    /// The raw/ partition to read, YYYY-MM-DD. Required, never defaulted from the clock:
    /// rebuilding an older date is the same operation as building today's.
    #[arg(long, value_name = "YYYY-MM-DD")]
    ingest_date: String,
    /// The observed_date to write (default: --ingest-date). They differ only when a fetch
    /// crossed UTC midnight.
    #[arg(long, value_name = "YYYY-MM-DD")]
    observed_date: Option<String>,
    /// Build from runs whose manifest says they did not finish. The result is not a whole day
    /// and the snapshot records that.
    #[arg(long)]
    allow_incomplete: bool,
    /// Read and parse raw, write nothing.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let ingest = chrono::NaiveDate::parse_from_str(&cli.ingest_date, "%Y-%m-%d")
        .context("--ingest-date")?
        .to_string();
    let observed = match &cli.observed_date {
        Some(d) => chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
            .context("--observed-date")?
            .to_string(),
        None => ingest.clone(),
    };

    let zone = dumpster_kit_lake::open_reader(&pkdump_site::lake_env())?;
    println!("==> raw landing zone: {}", zone.describe());
    let (rows, sealed, provenance, product_runs) = read(&zone, &ingest, &observed, &cli)?;

    if cli.dry_run {
        println!("==> --dry-run: nothing written");
        return Ok(());
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(write(&observed, rows, sealed, provenance, product_runs))
}

type Read = (
    Vec<prices::PriceRow>,
    Vec<prices::SealedPriceRow>,
    std::collections::BTreeMap<String, String>,
    Vec<String>,
);

fn read(zone: &RawZone, ingest: &str, observed: &str, cli: &Cli) -> anyhow::Result<Read> {
    use pkdump_site::{PRICES, PRODUCTS, TCGCSV};
    println!("==> reading source=tcgcsv dataset=products ingest_date={ingest}");
    let product_runs =
        prices_core::select_runs(&zone.runs(TCGCSV, PRODUCTS, ingest)?, cli.allow_incomplete)?;
    let mut fetch = |p: &PartRecord| -> anyhow::Result<Vec<u8>> { Ok(zone.payload(p)?) };
    let (sealed_ids, product_run_ids) =
        prices_core::sealed_product_ids(&product_runs, ingest, &mut fetch)?;

    println!("==> reading source=tcgcsv dataset=prices ingest_date={ingest}");
    let price_runs =
        prices_core::select_runs(&zone.runs(TCGCSV, PRICES, ingest)?, cli.allow_incomplete)?;
    let day = prices_core::read_date(&price_runs, ingest, &sealed_ids, &mut fetch)?;
    println!(
        "    {} price rows for observed_date={observed}",
        day.rows.len()
    );
    println!(
        "    {} sealed price row(s) for catalog.sealed_prices — resolved to one row per \
         product/price type, which is not the same count as the sealed rows in catalog.prices \
         (a sealed product quoted under two sub-types keeps both there)",
        day.sealed.len()
    );
    Ok((day.rows, day.sealed, day.provenance, product_run_ids))
}

async fn write(
    observed: &str,
    rows: Vec<prices::PriceRow>,
    sealed: Vec<prices::SealedPriceRow>,
    provenance: std::collections::BTreeMap<String, String>,
    product_runs: Vec<String>,
) -> anyhow::Result<()> {
    let config = LakeCatalogConfig::from_env()?;
    if config.reference.contains('@') {
        bail!(
            "PKDUMP_LAKE_REF must be a branch to write, got {:?}",
            config.reference
        );
    }
    let cat = dumpster_kit_iceberg::open(&config).await?;
    let (prices_table, sealed_table) = prices::ensure_tables(&cat).await?;

    // A day that already holds rows is REPLACED, as the Python job's overwrite does: one
    // commit per table, the rows of that observed_date swapped for the new ones, every other
    // day untouched. The first build of a date is the same call with nothing to remove.
    let days = replace::days_since_epoch(observed)?;
    let sealed_provenance = {
        let mut p = provenance.clone();
        p.insert("pkdump.raw-products-runs".into(), product_runs.join(","));
        p
    };
    let batch = replace::prices_batch(&prices_table, &rows, days)?;
    let n = rows.len();
    let replaced = replace::replace_day(
        &config,
        &prices_table,
        days,
        batch,
        provenance.clone().into_iter().collect(),
    )
    .await?;
    println!("==> wrote {n} rows to catalog.prices (replaced {replaced})");
    let batch = replace::sealed_batch(&sealed_table, &sealed, days)?;
    let m = sealed.len();
    let replaced = replace::replace_day(
        &config,
        &sealed_table,
        days,
        batch,
        sealed_provenance.into_iter().collect(),
    )
    .await?;
    println!("==> wrote {m} rows to catalog.sealed_prices (replaced {replaced})");

    println!(
        "    nessie commit {}   (ref {})",
        head_hash(&config).await?,
        config.reference
    );
    println!("    provenance     {provenance:?}");
    Ok(())
}

/// The Nessie commit at the tip of the ref: the one value that addresses the whole catalog.
async fn head_hash(config: &LakeCatalogConfig) -> anyhow::Result<String> {
    let base = config.uri.trim_end_matches('/');
    let Some(root) = base.strip_suffix("/iceberg") else {
        bail!(
            "expected an Iceberg REST endpoint ending in /iceberg, got {:?}",
            config.uri
        );
    };
    let url = format!("{root}/api/v2/trees/{}", config.reference);
    let body: serde_json::Value = reqwest::get(&url).await?.error_for_status()?.json().await?;
    body["reference"]["hash"]
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("{url}: no reference.hash in the answer"))
}
