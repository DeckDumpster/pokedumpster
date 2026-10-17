//! `pkdump-lake-prices-age` — how far behind is the newest catalog.prices
//! partition? Exit 0 fresh, 3 stale, 1 could not ask.

use chrono::NaiveDate;
use clap::Parser;
use pkdump_lakejobs::freshness;

#[derive(Parser)]
#[command(name = "pkdump-lake-prices-age")]
struct Args {
    /// The date to measure the newest partition against, YYYY-MM-DD. Required,
    /// never defaulted from the clock: the scheduler knows what day it is.
    #[arg(long)]
    as_of: NaiveDate,
    #[arg(long, default_value_t = freshness::DEFAULT_MAX_AGE_DAYS, value_parser = clap::value_parser!(i64).range(0..))]
    max_age_days: i64,
    #[arg(long, default_value = freshness::DEFAULT_TABLE)]
    table: String,
    /// Nessie ref to read (default $PKDUMP_LAKE_REF, then main)
    #[arg(long)]
    r#ref: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let v = freshness::run(&a.table, a.as_of, a.max_age_days, a.r#ref.as_deref()).await?;
    v.stdout.iter().for_each(|l| println!("{l}"));
    v.stderr.iter().for_each(|l| eprintln!("{l}"));
    std::process::exit(v.code);
}
