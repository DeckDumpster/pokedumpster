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
//! [`pkdump_derive::derive`], the body `pkdump data refresh` used to run
//! inline, moved out of the CLI unchanged. All this binary supplies is where
//! the bytes come from ([`replay`]) and which bytes those are ([`partition`]).
//! That is what made item 3's comparison meaningful: two runs of ONE
//! derivation over two sources of the SAME bytes. Two implementations agreeing
//! would only ever have been evidence about the second implementation.
//!
//! Since item 6 (pd-lunn) the refresh calls [`pkdump_derive::land`] — that
//! same acquisition, no imports — and this binary is `derive`'s only caller
//! left. So that comparison can no longer be run at all: there is no second
//! builder to diff against, which was the point of deleting one.
//!
//! ## What it refuses
//!
//! - a date whose partition was never landed, or landed incompletely — it
//!   never falls back to the newest available date, because "yesterday's raw
//!   silently deriving today's catalog" is the failure this whole design is
//!   arranged against. **One incompleteness is exempt**: a partition short
//!   only in the pokemontcg.io tail derives and exits 2 rather than refusing,
//!   see below;
//! - a partition whose manifests carry no clock, or disagree about it;
//! - a URL the partition has no record of. There is no fallback to the live
//!   upstream: a derive that fetched what `raw/` did not hold would produce a
//!   correct catalog whose lineage is not reproducible, which is the one
//!   failure the landing zone exists to prevent. The temporary fallback item 2
//!   shipped with, and its `--no-upstream-fallback` opt-out, are gone (item 4).
//!   Set-symbol normalisation is not an exception to this — it never came
//!   through the replay layer at all, images being deliberately outside `raw/`;
//! - `--ingest-date` defaulted from the wall clock. There is no default.
//!   Deriving an older date is the same operation as deriving today's, and a
//!   job that reads the clock has two behaviours where it should have one.
//!   The *scheduler* is the component allowed to know what day it is, so
//!   `deploy/derive.sh` names the date explicitly — exactly as
//!   `pkdump-lake-value-snapshots` already requires.
//!
//! ## 0 / 2 / 1 are three different answers (pd-llbq)
//!
//! | | |
//! | --- | --- |
//! | **0** | the catalog is the derivation of that partition |
//! | **2** | it is the derivation of a **partial** partition: the pokemontcg.io tail did not complete, so the set list is as old as the last run that finished one |
//! | **1** | there is no catalog for that partition — absent, short in TCGCSV, clockless, a URL `raw/` has no record of, or the derivation itself failed |
//!
//! Exit 2 exists because `pkdump data refresh` already answers a night
//! upstream is down that way (pd-nons) and this job was answering the same
//! night with a refusal and a page. Two units taking opposite policies on one
//! upstream's weather is not a decision anybody made; it is what falls out of
//! two beads landing beside each other. What settles it is which mistake is
//! cheaper: paging most nights trains the pager to be ignored (pd-me6h), and
//! once this job is the only builder of `shared.sqlite` — epic item 6 —
//! refusing the night also throws away its **prices**, which is the one thing
//! that cannot be re-fetched tomorrow. So: derive it, say PARTIAL, and let the
//! provenance rows carry `complete: false` for the half that was short.
//!
//! What exit 2 is NOT is a licence for a smaller catalog generally. Every
//! other short prefix is still exit 1: see `partition::requirement`, where the
//! exemption is spelled out per dataset and the compiler makes adding one a
//! decision.

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
    let plan = partition::choose(&zone, &args.ingest_date)?;
    let clock = partition::clock_of(&plan.chosen, &args.ingest_date)?;
    println!(
        "  clock {} (observed {}) — recovered from the run's manifests, not read here",
        clock.fetched_at(),
        clock.observed_date()
    );

    // Said before the derive rather than after it, because the derive is
    // minutes of progress lines and this is what an operator needs in order to
    // read the ones that follow — the tail is about to fail again, at the same
    // request it failed at when the partition was landed, and that is the
    // expected shape rather than a new fault.
    if plan.is_partial() {
        eprintln!(
            "!! PARTIAL PARTITION: ingest_date={} did not land whole. The pokemontcg.io tail \
             was short:",
            args.ingest_date
        );
        for line in &plan.partial {
            eprintln!("!!   {line}");
        }
        eprintln!(
            "!! Deriving anyway. The night's TCGCSV half — the half that cannot be re-fetched \
             — is whole, and the set list will be as old as the last run that finished one. \
             This run will exit 2."
        );
    }

    let replay = Arc::new(replay::RawReplay::new(zone, &plan.chosen)?);
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

    // A tail that failed on a partition the landing zone says is WHOLE is a
    // fault rather than a partial night: every URL the tail asked for is in
    // `raw/`, so the failure came from replaying them — a corrupt payload, or
    // a derivation that grew a request the fetch never made. That is exit 1,
    // and it is the behaviour this job has always had.
    //
    // A partition that is short in the tail is the other case, and it is the
    // one pd-llbq is about. It is not a failure: it derives, records honest
    // provenance, and exits 2 like `pkdump data refresh` does on that same
    // night. See the module docs for why the two units must agree here.
    if let Some(e) = &report.tail_error
        && !plan.is_partial()
    {
        anyhow::bail!(
            "the pokemontcg.io tail did not complete, so this catalog is NOT the derivation of \
             ingest_date={}: {e}\n\
             The partition itself landed WHOLE — every URL the tail asked for is in raw/ — so \
             this is not the partial night a short tail prefix would be. Something failed on \
             the way back OUT of the landing zone.",
            args.ingest_date
        );
    }

    // Provenance, written for a partial run too: `raw_derivation.complete`
    // carries each dataset's own answer, so a row saying this catalog came
    // from that run is a claim about a catalog that exists — and the record of
    // WHICH half of that night was short is exactly what an operator reading
    // back a stale set list needs. Not written when the run failed outright,
    // because there is then no catalog for it to describe.
    let rows = partition::provenance(&plan.chosen, &args.ingest_date, &clock);
    let derived_at = chrono::Utc::now().to_rfc3339();
    pkdump_db::raw_derivation::record(&mut conn, &args.ingest_date, &derived_at, &rows)?;
    println!(
        "  provenance: {} partition(s) recorded in raw_derivation for {}",
        rows.len(),
        args.ingest_date
    );

    // Reaching here already means it: a URL outside the partition is a refusal
    // (see `replay`), so a run that finished was answered from `raw/` alone. Said
    // out loud anyway, because it is what an operator reads to know the lineage
    // is intact — and because the phase that still fetches, set-symbol
    // normalisation, prints its own line just above and would otherwise read as
    // a hole in this claim.
    //
    // Not said on a partial night, and the difference is not cosmetic: the run
    // asked for a URL the partition does not hold. It holds a *failure record*
    // for it rather than nothing at all, which is why the run is a partial
    // night and not a coverage regression — but "every upstream request was
    // answered from raw/" would be false, and this line is read as a claim.
    if plan.is_partial() {
        eprintln!(
            "!! PARTIAL DERIVATION: {} is derived from ingest_date={}, whose pokemontcg.io tail \
             did not complete. Its TCGCSV half is whole; its set list is as old as the last run \
             that finished one. raw_derivation records which datasets were short. Exit status 2.",
            db_path.display(),
            args.ingest_date
        );
        println!(
            "Derive PARTIAL: {} ({} printings, {} latest prices)",
            db_path.display(),
            report.printings,
            report.latest_prices
        );
        // Not an `Err`: anyhow's main would print it and exit 1, which is the
        // status a run that produced NO catalog carries. This one produced a
        // catalog, and said what is missing from it. Same shape, same reason,
        // as `pkdump data refresh` (crates/pkdump-cli/src/data.rs).
        reclaim(&conn);
        drop(conn);
        std::process::exit(2);
    }

    println!("  raw coverage: complete — every upstream request was answered from raw/");

    println!(
        "Derive complete: {} ({} printings, {} latest prices)",
        db_path.display(),
        report.printings,
        report.latest_prices
    );
    reclaim(&conn);
    Ok(())
}

/// Give the catalog's WAL back, after this job's LAST write and before it exits
/// (pd-t50h).
///
/// Both exit paths call it, and the placement is the whole of the claim. A
/// checkpoint at the end of `pkdump_derive::derive` would run before
/// `raw_derivation::record`, leaving the provenance row's frames in a file
/// nothing will ever truncate — an autocheckpoint runs on a commit, this
/// process is the only thing that commits to the catalog, and it is about to
/// exit. So the `-wal` a night leaves behind sits on the data volume until the
/// next night's run. `row_identical.rs::a_derive_leaves_no_wal_behind_on_the_
/// catalog_it_built` asserts the file is empty against the shipped binary, and
/// that assertion is what found the ordering.
///
/// Not fallible from here. `reclaim_catalog_wal` already reports a checkpoint a
/// reader blocked rather than raising — the catalog is complete either way, and
/// a browsing session must not be able to fail the nightly build — so the only
/// thing left to swallow is a checkpoint that could not run at all, which
/// changes nothing about the catalog this run just wrote and must not turn a
/// good derivation into exit 1.
fn reclaim(conn: &rusqlite::Connection) {
    if let Err(e) = pkdump_db::reclaim_catalog_wal(conn) {
        eprintln!(
            "!! could not checkpoint the catalog's WAL: {e}. Disk only — the catalog is complete."
        );
    }
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
