//! `pkdump-lake-derive` — the offline catalog derive.
//!
//! ```text
//!   pkdump-lake-derive shared --ingest-date 2026-08-11 --db /data/shared.sqlite
//!   pkdump-lake-derive diff --left a.sqlite --right b.sqlite --exclude raw_derivation
//! ```
//!
//! ## Why this is a separate binary
//!
//! > Only lakehouse code reads `raw/`. The shared and tenant databases are
//! > derived from that, so whatever produces them is **also** lakehouse code.
//!
//! That rule is how the offline and online processes decouple onto different
//! machines, and it is the reason this is a crate rather than a `--from-raw`
//! flag on `pkdump data refresh`: a flag would put a raw reader inside
//! `pkdump-cli`, on the **online** side, which is precisely the coupling the
//! rule exists to break. This crate is a binary with no library, so nothing
//! online can link it even by accident — and `pkdump-cli` does not depend on
//! it, which is the mechanical version of the same statement.
//!
//! What it is **not** is a second derivation. The pipeline it runs is
//! [`pkdump_derive::derive`], the same function `pkdump data refresh` calls,
//! moved out of the CLI unchanged. All this binary supplies is where the bytes
//! come from ([`replay`]) and which bytes those are ([`partition`]). That is
//! what makes item 3's comparison meaningful: two runs of ONE derivation over
//! two sources of the SAME bytes. Two implementations agreeing would only ever
//! be evidence about the second implementation.
//!
//! ## What it refuses
//!
//! - a date whose partition was never landed, or landed incompletely — it
//!   never falls back to the newest available date, because "yesterday's raw
//!   silently deriving today's catalog" is the failure this whole design is
//!   arranged against;
//! - a partition whose manifests carry no clock, or disagree about it;
//! - `--ingest-date` defaulted from the wall clock. There is no default.
//!   Deriving an older date is the same operation as deriving today's, and a
//!   job that reads the clock has two behaviours where it should have one.
//!   The *scheduler* is the component allowed to know what day it is, so
//!   `deploy/derive.sh` names the date explicitly — exactly as
//!   `pkdump-lake-value-snapshots` already requires.

mod diff;
mod partition;
mod replay;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

/// The offline half of the catalog: derive `shared.sqlite` from `raw/`.
#[derive(Parser)]
#[command(name = "pkdump-lake-derive", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Derive the shared catalog from one `raw/` partition.
    Shared(SharedArgs),
    /// Compare two catalogs row by row. The acceptance test's instrument.
    Diff(DiffArgs),
}

#[derive(clap::Args)]
struct SharedArgs {
    /// The `raw/` partition to derive from, `YYYY-MM-DD`.
    ///
    /// Required, with no default from the clock — see the module docs.
    #[arg(long, value_name = "YYYY-MM-DD")]
    ingest_date: String,

    /// The shared catalog to derive into (default: $PKDUMP_HOME/shared.sqlite).
    ///
    /// The derive is incremental, exactly as the online refresh is: it updates
    /// the catalog it is given rather than building one from nothing.
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,

    /// Where set-symbol PNGs are cached. Defaults to the catalog's directory.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,

    /// Refuse a URL that is not in `raw/` instead of fetching it live.
    ///
    /// The fallback ships in item 2 of the epic and is removed in item 4, as
    /// its own change, once row-identical is proven. This flag is how a gate
    /// (or an operator who wants the guarantee today) gets item 4's behaviour
    /// early. With the fallback ON a miss is still loud and still recorded —
    /// what it is not is fatal.
    #[arg(long)]
    no_upstream_fallback: bool,
}

#[derive(clap::Args)]
struct DiffArgs {
    /// The catalog on the left of the comparison.
    #[arg(long, value_name = "PATH")]
    left: PathBuf,
    /// The catalog on the right.
    #[arg(long, value_name = "PATH")]
    right: PathBuf,
    /// A table to skip. Repeatable, and echoed in the report — an exclusion
    /// nobody can see is how a comparator starts proving nothing.
    #[arg(long, value_name = "TABLE")]
    exclude: Vec<String>,
    /// How many differing rows to print per table.
    #[arg(long, default_value_t = 5)]
    max_diffs: usize,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Shared(args) => shared(args),
        Command::Diff(args) => diff_cmd(args),
    }
}

/// Derive `shared.sqlite` from one raw partition.
fn shared(args: SharedArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    let data_dir = args.data_dir.unwrap_or_else(|| {
        db_path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    });

    // The lake, opened for READING. `open_reader` hands back a handle with no
    // `put` on it at all: `raw/` is immutable, and the job that derives from
    // the evidence must not be able to rewrite it.
    let zone = pkdump_lake::open_reader()?;
    println!(
        "Deriving ingest_date={} from {}",
        args.ingest_date,
        zone.describe()
    );

    // Which runs, and the refusals. Before anything opens the catalog: a date
    // that cannot be derived must not leave a half-written one behind.
    let chosen = partition::choose(&zone, &args.ingest_date)?;
    let clock = partition::clock_of(&chosen, &args.ingest_date)?;
    println!(
        "  clock {} (observed {}) — recovered from the run's manifests, not read here",
        clock.fetched_at(),
        clock.observed_date()
    );

    let replay = Arc::new(replay::RawReplay::new(
        zone,
        &chosen,
        !args.no_upstream_fallback,
    )?);
    println!("  {} URL(s) replayable from this partition", replay.urls());

    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    let report = pkdump_derive::derive(
        &mut conn,
        &pkdump_derive::Options {
            clock: clock.clone(),
            data_dir: &data_dir,
            // The offline job never lands. Landing is the other unit's job,
            // and a derive that re-landed what it just read would be writing
            // a second copy of its own input.
            landing: None,
            replay: Some(replay.clone() as Arc<dyn pkdump_ingest::landing::ReplaySource>),
        },
    )?;

    // A partial derivation is a failure HERE, unlike in `pkdump data refresh`
    // (pd-nons). The online refresh may end with a stale set list because the
    // dataset it could not lose — a day's prices — is already in the catalog
    // by then, and it says so with exit status 2. This job makes a different
    // claim: that `shared.sqlite` is what this partition derives to. A catalog
    // missing the sets the partition holds does not answer that claim, and the
    // unit that runs this has no SuccessExitStatus= precisely because a
    // quietly smaller catalog reads as cards that do not exist.
    if let Some(e) = &report.tail_error {
        anyhow::bail!(
            "the pokemontcg.io tail did not complete, so this catalog is NOT the derivation of \
             ingest_date={}: {e}",
            args.ingest_date
        );
    }

    // Provenance, written only on success: a row saying this catalog came from
    // that run is a claim about a catalog that exists.
    let rows = partition::provenance(&chosen, &args.ingest_date, &clock);
    let derived_at = chrono::Utc::now().to_rfc3339();
    pkdump_db::raw_derivation::record(&mut conn, &args.ingest_date, &derived_at, &rows)?;
    println!(
        "  provenance: {} partition(s) recorded in raw_derivation for {}",
        rows.len(),
        args.ingest_date
    );

    // The fallback's summary. Loud at the point of use (see `replay`) and loud
    // again here, because the per-URL lines scroll past in a run that prints a
    // progress line per thousand cards.
    let misses = replay.misses();
    if misses.is_empty() {
        println!("  raw coverage: complete — every upstream request was answered from raw/");
    } else {
        eprintln!(
            "!! raw coverage has REGRESSED: {} URL(s) were not in raw/ and were fetched live:",
            misses.len()
        );
        for url in misses.iter().take(20) {
            eprintln!("!!   {url}");
        }
        if misses.len() > 20 {
            eprintln!("!!   … and {} more", misses.len() - 20);
        }
        eprintln!(
            "!! This derive is NOT reproducible from the lake. Land the missing endpoints, \
             re-land the date, and derive again."
        );
    }

    println!(
        "Derive complete: {} ({} printings, {} latest prices)",
        db_path.display(),
        report.printings,
        report.latest_prices
    );
    Ok(())
}

/// Compare two catalogs row by row.
fn diff_cmd(args: DiffArgs) -> anyhow::Result<()> {
    // Read-only, both of them: a comparison that could write is a comparison
    // that can change its own answer. `query_only` rather than SQLITE_OPEN_
    // READ_ONLY because these are WAL databases and opening one read-only
    // fails outright on a directory SQLite cannot create a -shm in.
    let left = open_query_only(&args.left)?;
    let right = open_query_only(&args.right)?;

    println!(
        "Comparing {} (left) with {} (right)",
        args.left.display(),
        args.right.display()
    );
    let report = diff::compare(&left, &right, &args.exclude, args.max_diffs)?;
    report.print();

    if report.matched() {
        println!("ROW-IDENTICAL: every compared table matches, row for row.");
        Ok(())
    } else {
        anyhow::bail!("the two catalogs are NOT row-identical — see the DIFF lines above")
    }
}

fn open_query_only(path: &std::path::Path) -> anyhow::Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(path)?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}
