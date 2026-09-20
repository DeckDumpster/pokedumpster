//! `pkdump data refresh` — the nightly LANDING run.
//!
//! It fetches the tail of newest sets from the pokemontcg.io API and every
//! TCGCSV group's products and prices, and writes each response into the
//! `raw/` landing zone. It builds nothing: since pd-lunn the catalog has ONE
//! builder, `pkdump-lake-derive shared`, which replays this partition some
//! hours later. `deploy/pkdump-derive.timer` is what runs it, and on a box
//! running this command it is not optional.
//!
//! What is left in this file is the online half of a run: which database to
//! read, where the bytes go, and the clock. **Nothing here reads `raw/`** —
//! that is the boundary the lakehouse epic turns on, and it is why the
//! deriving job is a different binary in a different crate rather than a
//! `--from-raw` flag on this one.

use std::path::PathBuf;

use pkdump_ingest::{coverage, overrides, pokemon_tcg_data, symbols};

/// The `pkdump data` subcommand group.
#[derive(clap::Args)]
pub struct DataArgs {
    #[command(subcommand)]
    command: DataCommand,
}

#[derive(clap::Subcommand)]
enum DataCommand {
    /// Fetch every upstream and land it in `raw/`. Builds no catalog —
    /// `pkdump-lake-derive shared` does that, from the partition this run
    /// lands (pd-lunn).
    Refresh(RefreshCmdArgs),
    /// Trim and resize set symbol glyphs in isolation — useful for
    /// migrating an existing catalog without paying for a full TCGCSV
    /// import.
    NormalizeSymbols(RefreshArgs),
    /// Reconcile variant + sub_type maps from JSON and re-run variant
    /// expansion against existing TCGCSV products. No network — useful
    /// after editing data/variants.json or data/tcgcsv_sub_type_variants
    /// .json, or after a migration that adds a new bridge.
    Expand(RefreshArgs),
    /// Re-apply `data/overrides/upstream_card_corrections.json` to cards
    /// already in the catalog. `upsert_card` only corrects rows as they
    /// are ingested and `refresh` skips sets it already has, so a
    /// correction added (or edited) after the fact needs this pass to
    /// reach the existing row. No network; idempotent.
    ApplyCorrections(ApplyCorrectionsArgs),
    /// One-time: reconstruct collection value history from `shared.prices`
    /// × each copy's acquisition + status history, into the user DB's
    /// `collection_value_snapshot` table. `pkdump-lake-value-snapshots`
    /// records today going forward, for every tenant; this seeds the past
    /// for the one `$PKDUMP_USER` names. Idempotent.
    BackfillValueHistory(RefreshArgs),
}

/// Arguments shared by the `pkdump data` subcommands that only need a
/// database.
#[derive(clap::Args)]
pub struct RefreshArgs {
    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
}

/// Arguments for `pkdump data refresh`.
///
/// **There is no `--land-raw` any more.** Landing was the optional half of a
/// command whose other half built the catalog; with that half deleted, a run
/// that does not land does nothing at all, so landing is unconditional and
/// `~/.config/pkdump/lake.env` is required. A flag left behind for a path that
/// no longer exists is a flag a runbook can still reach for.
#[derive(clap::Args)]
pub struct RefreshCmdArgs {
    #[command(flatten)]
    common: RefreshArgs,
}

/// Arguments for `pkdump data apply-corrections`.
#[derive(clap::Args)]
pub struct ApplyCorrectionsArgs {
    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
    /// Report the rows that would change without writing them.
    #[arg(long)]
    dry_run: bool,
}

/// Execute `pkdump data`.
pub fn run(args: DataArgs) -> anyhow::Result<()> {
    match args.command {
        DataCommand::Refresh(args) => refresh(args),
        DataCommand::NormalizeSymbols(args) => normalize_symbols(args),
        DataCommand::Expand(args) => expand_only(args),
        DataCommand::ApplyCorrections(args) => apply_corrections(args),
        DataCommand::BackfillValueHistory(args) => backfill_value_history(args),
    }
}

/// Execute `pkdump data apply-corrections` — heal already-ingested rows
/// against the upstream-correction registry.
fn apply_corrections(args: ApplyCorrectionsArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let conn = pkdump_db::open_shared(&db_path)?;

    let rows = if args.dry_run {
        pokemon_tcg_data::pending_corrections(&conn)?
    } else {
        pokemon_tcg_data::apply_corrections_to_db(&conn)?
    };
    let verb = if args.dry_run {
        "would change"
    } else {
        "healed"
    };
    for r in &rows {
        println!(
            "  {} number {} -> {} (sortable {} -> {})",
            r.card_id,
            r.current_number,
            r.corrected_number,
            r.current_number_sortable,
            r.corrected_number_sortable
        );
    }
    println!("{} row(s) {verb}.", rows.len());
    Ok(())
}

/// Execute `pkdump data backfill-value-history` — a one-time reconstruction
/// of the collection's value over time. Unlike `refresh`/`setup` (which open
/// the *shared* catalog read-write), value snapshots live in the *user* DB,
/// so this opens a user connection (`connect_user`) with the shared catalog
/// attached read-only — collection value needs the user's copies.
fn backfill_value_history(args: RefreshArgs) -> anyhow::Result<()> {
    let shared_db = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    let user_db = crate::collection::user_db()?;
    println!(
        "Backfilling value history into {} (catalog {})",
        user_db.display(),
        shared_db.display()
    );
    let mut conn = pkdump_db::connect_user(&user_db, &shared_db)?;
    let rows = pkdump_db::value_history::backfill(&mut conn)?;
    println!("Value-history backfill complete: {rows} snapshot rows.");
    Ok(())
}

/// Execute `pkdump data expand` — reconcile JSON-driven lookups and
/// re-run variant expansion. Skips all network steps. Use after editing
/// `data/variants.json` or `data/tcgcsv_sub_type_variants.json`, or
/// after a migration that adds a bridge entry the catalog doesn't yet
/// have linked in `tcgplayer_groups`.
fn expand_only(args: RefreshArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    println!("Reconciling variants table from data/variants.json...");
    let n_variants = pkdump_db::variants::reconcile(&mut conn)?;
    println!("  {n_variants} variant rows reconciled");

    println!("Reconciling tcgcsv_sub_type_variant_map...");
    let n_sub = pkdump_db::sub_type_map::reconcile(&mut conn)?;
    println!("  {n_sub} (group, sub_type) → variant rows");

    println!("Reconciling bundles table from data/bundles.json...");
    let n_bundles = pkdump_db::bundles::reconcile(&mut conn)?;
    println!("  {n_bundles} bundles registered");

    // Re-run set discovery too: it's local (it reads the TCGCSV products
    // already in the DB), so editing data/overrides/tcgcsv_set_discovery
    // .json takes effect here without a network refresh.
    println!("Discovering new sets from unbridged TCGCSV groups...");
    for d in pkdump_ingest::set_discovery::discover_new_sets(&mut conn)? {
        println!(
            "  {} ({}) — {} from group {}, {} cards",
            d.set_code, d.series, d.name, d.group_id, d.cards
        );
    }

    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    // `data expand` is the local re-run, so its clock is genuinely now: there
    // are no landed bytes behind it to reproduce a timestamp from.
    let printings =
        overrides::expand_all_printings(&mut conn, &overlay, &chrono::Utc::now().to_rfc3339())?;
    println!("  wrote {printings} printings");

    println!("Checking TCGplayer mapping coverage...");
    coverage::report_unmapped_sets(&conn)?;
    Ok(())
}

/// Execute `pkdump data normalize-symbols` — just the symbols phase, no
/// network catalog work.
fn normalize_symbols(args: RefreshArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    let mut conn = pkdump_db::open_shared(&db_path)?;
    let data_dir = db_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    println!("Normalizing set symbol glyphs...");
    let s = symbols::normalize_all_symbols(&mut conn, &data_dir)?;
    println!(
        "  {} processed, {} cached, {} overrides, {} failed",
        s.processed, s.cached, s.overrides, s.failed
    );
    Ok(())
}

/// Raw-landing coverage: every upstream input a refresh consumes reaches
/// `raw/`, on an ordinary night and not just the first one.
///
/// This is a gate, not a review. The gap it was written for was invisible to
/// reading the code: every fetch in the acquisition phase *does* go through
/// `landing::fetch_bytes`, so a call-site audit passes — but `import_tail`
/// only asks for a set's cards when the catalog lacks the set, so on every
/// night after the first, `dataset=cards` was never requested and therefore
/// never landed (pd-v1ca). A lake missing the cards corpus cannot derive
/// `shared.sqlite`, and nothing said so: the refresh succeeded, the manifests
/// were complete, and the dataset simply was not there.
///
/// The cards gap is now covered by the pokemon-tcg-data bulk corpus (db-9ogb):
/// one tarball landing as `Dataset::Bulk` carries every set and card, so a
/// cold rebuild can derive without the pokemontcg.io tail's cards. The tail
/// still covers the 2–3 month lag window between a set's publication and the
/// bulk repo catching up.
///
/// So the gate runs two acquisitions against a fake upstream — night one to
/// fill the catalog, night two to be the ordinary night — and audits the
/// second:
///
/// 1. every `Dataset` the refresh is responsible for has a complete prefix
///    with parts in it, walked from [`Dataset::ALL`] so a dataset added later
///    cannot be forgotten,
/// 2. every request the upstream served was landed, exactly once — the
///    call-site audit, done by comparing what was asked for against what was
///    stored rather than by reading,
/// 3. the TCGCSV prefixes carry both categories: English (3) and Pokémon
///    Japan (85) share `source=tcgcsv`, which is why a bucket listing shows
///    no Japanese dataset and why "Japan is not landed" is easy to conclude
///    from the outside. It is landed; this pins that.
#[cfg(test)]
mod raw_coverage {
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::{Mutex, MutexGuard};

    use pkdump_ingest::test_upstream::{FakeUpstream, Reply};
    use pkdump_ingest::upstream::{
        ENV_POKEMON_TCG_DATA_BASE_URL, ENV_POKEMONTCG_BASE_URL, ENV_TCGCSV_BASE_URL,
    };
    use pkdump_lake::{Dataset, DirStore, Manifest, RawLanding};

    const INGEST_DATE: &str = "2026-09-20";
    const CLOCK_AT: &str = "2026-09-20T06:00:00Z";

    /// Where a dataset's bytes come from, for the audit below.
    enum Coverage {
        /// Every landing-enabled refresh lands it, every night.
        Refresh,
        /// Only when new sets are published by pokemontcg.io — the lag window
        /// between publication and the bulk corpus catching up. On most nights
        /// the catalog already has every set the tail finds, so nothing lands
        /// here. The pokemon-tcg-data bulk corpus covers the full cards history
        /// for cold rebuilds (db-9ogb).
        TailLagWindow(&'static str),
    }

    /// The match is exhaustive on purpose: a new [`Dataset`] does not compile
    /// until somebody says whether a refresh has to land it. That is what
    /// makes this a gate against the *next* gap rather than a fix for this
    /// one.
    fn coverage(dataset: Dataset) -> Coverage {
        match dataset {
            Dataset::Sets
            | Dataset::Groups
            | Dataset::Products
            | Dataset::Prices
            | Dataset::Bulk => Coverage::Refresh,
            Dataset::Cards => Coverage::TailLagWindow(
                "the pokemontcg.io tail fetches cards only when a new set has been published \
                 and the catalog lacks it — on most nights none are missing. The \
                 pokemon-tcg-data bulk corpus (Dataset::Bulk) covers the full cards history \
                 for cold rebuilds; the tail covers only the 2–3 month lag window before \
                 the bulk repo catches up (pd-v1ca, db-9ogb)",
            ),
        }
    }

    // One set, one English group, one Japanese group: enough that every
    // endpoint the acquisition phase knows how to call gets called.
    const SETS: &str = r#"{"data":[
        {"id":"sv3pt5","name":"151","series":"Scarlet & Violet",
         "printedTotal":165,"total":207,"ptcgoCode":"MEW",
         "releaseDate":"2023/09/22"}],
        "page":1,"pageSize":250,"count":1,"totalCount":1}"#;
    const CARDS: &str = r#"{"data":[
        {"id":"sv3pt5-4","name":"Charmander","supertype":"Pokémon",
         "subtypes":["Basic"],"hp":"60","types":["Fire"],"number":"4",
         "rarity":"Common",
         "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"},
         "tcgplayer":{"prices":{"normal":{"market":0.5}}}}],
        "page":1,"pageSize":250,"count":1,"totalCount":1}"#;
    const ENGLISH_GROUPS: &str = r#"{"results":[
        {"groupId":23237,"name":"SV: 151","abbreviation":"MEW",
         "publishedOn":"2023-09-22"}],"success":true,"errors":[]}"#;
    const JAPAN_GROUPS: &str = r#"{"results":[
        {"groupId":23099,"name":"SV2a: Pokemon Card 151","abbreviation":"",
         "publishedOn":"2023-06-16"}],"success":true,"errors":[]}"#;
    const EMPTY: &str = r#"{"results":[],"success":true,"errors":[]}"#;

    /// Both upstreams on one server — the TCGCSV origin is the root, the
    /// pokemontcg.io one is `/v2`, and the bulk tarball is under
    /// `/PokemonTCG/...`, exactly as the real hosts are shaped.
    fn route(target: &str, _n: usize) -> Reply {
        let path = target.split('?').next().unwrap_or(target);
        match path {
            "/3/groups" => Reply::ok(ENGLISH_GROUPS),
            "/85/groups" => Reply::ok(JAPAN_GROUPS),
            "/v2/sets" => Reply::ok(SETS),
            "/v2/cards" => Reply::ok(CARDS),
            p if p.ends_with("/products") || p.ends_with("/prices") => Reply::ok(EMPTY),
            // The pokemon-tcg-data bulk tarball — landed as bytes, never
            // parsed during a `pkdump data refresh` run.
            p if p.contains("pokemon-tcg-data") => Reply {
                status: 200,
                body: b"bulk-placeholder".to_vec(),
                content_type: "application/x-tar",
            },
            other => Reply::status(
                404,
                format!(
                    r#"{{"error":"the acquisition phase asked for {other}, which this \
                        fixture does not model — a new upstream call needs a route here \
                        AND a landed dataset"}}"#
                ),
            ),
        }
    }

    /// Serialised: the origin overrides are process-wide, and they are the
    /// only way to point a whole acquisition phase somewhere (it builds its
    /// own clients — see `pkdump_ingest::upstream`).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct Origins<'a>(#[allow(dead_code)] MutexGuard<'a, ()>);

    impl Origins<'_> {
        fn point_at(base: &str) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: the lock is held for as long as the variables are set,
            // and this is the only test binary that touches them.
            unsafe {
                std::env::set_var(ENV_TCGCSV_BASE_URL, base);
                std::env::set_var(ENV_POKEMONTCG_BASE_URL, format!("{base}/v2"));
                std::env::set_var(ENV_POKEMON_TCG_DATA_BASE_URL, base);
            }
            Self(guard)
        }
    }

    impl Drop for Origins<'_> {
        fn drop(&mut self) {
            // SAFETY: as above — still under the lock this value holds.
            unsafe {
                std::env::remove_var(ENV_TCGCSV_BASE_URL);
                std::env::remove_var(ENV_POKEMONTCG_BASE_URL);
                std::env::remove_var(ENV_POKEMON_TCG_DATA_BASE_URL);
            }
        }
    }

    /// One landing-enabled derivation (fills the catalog and lands bytes).
    /// Night one: populate the catalog so an ordinary night can run.
    fn derive_landing(db: &Path, dir: &Path) {
        let clock =
            pkdump_derive::DeriveClock::from_manifest(CLOCK_AT, "test clock").expect("parse clock");
        let landing = Arc::new(RawLanding::new(
            Box::new(DirStore::new(dir)),
            INGEST_DATE,
            clock.fetched_at(),
        ));
        let mut conn = pkdump_db::open_shared(db).expect("open the catalog read-write");
        let outcome = pkdump_derive::derive(
            &mut conn,
            &pkdump_derive::Options {
                clock,
                data_dir: dir,
                landing: Some(Arc::clone(&landing)),
                replay: None,
            },
        );
        pkdump_derive::finalize_landing(&landing, outcome.as_ref().err())
            .expect("write the manifests");
        outcome.expect("the derivation");
    }

    /// One landing-enabled land run (read-only catalog, lands bytes only).
    /// Night two: the ordinary night where everything is already in the catalog.
    fn land_night(db: &Path, dir: &Path) -> Vec<Manifest> {
        let clock =
            pkdump_derive::DeriveClock::from_manifest(CLOCK_AT, "test clock").expect("parse clock");
        let landing = Arc::new(RawLanding::new(
            Box::new(DirStore::new(dir)),
            INGEST_DATE,
            clock.fetched_at(),
        ));
        let conn = pkdump_db::open_shared_readonly(db).expect("open the catalog read-only");
        let outcome = pkdump_derive::land(
            &conn,
            &pkdump_derive::Options {
                clock,
                data_dir: dir,
                landing: Some(Arc::clone(&landing)),
                replay: None,
            },
        );
        pkdump_derive::finalize_landing(&landing, outcome.as_ref().err())
            .expect("write the manifests");
        outcome.expect("the landing run");
        landing.manifests()
    }

    /// Request targets, in served order, with the origin stripped.
    fn targets(urls: impl IntoIterator<Item = String>, base: &str) -> Vec<String> {
        let mut out: Vec<String> = urls
            .into_iter()
            .map(|u| u.strip_prefix(base).unwrap_or(&u).to_string())
            .collect();
        out.sort();
        out
    }

    fn manifest_for(manifests: &[Manifest], dataset: Dataset) -> Option<&Manifest> {
        manifests.iter().find(|m| m.dataset == dataset.as_str())
    }

    /// The gate. See the module docs for what each assertion is for.
    #[test]
    fn an_ordinary_night_lands_every_dataset_the_catalog_is_derived_from() {
        let upstream = FakeUpstream::start(route);
        let _origins = Origins::point_at(&upstream.base_url());
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("shared.sqlite");

        // Night one: an empty catalog, so `import_tail` imports the set and
        // fetches its cards on the way past. This is the run that made the
        // gap invisible — raw/ looked complete the day the lake was built.
        derive_landing(&db, &tmp.path().join("night-1"));

        // Night two: every set is already in the catalog. The ordinary night,
        // and the one every night after the first looks like.
        let before = upstream.requests().len();
        let manifests = land_night(&db, &tmp.path().join("night-2"));

        // 1. Every dataset a refresh is responsible for, walked from the enum
        //    rather than from a list written here.
        for dataset in Dataset::ALL {
            match coverage(*dataset) {
                Coverage::Refresh => {
                    let landed = manifest_for(&manifests, *dataset).unwrap_or_else(|| {
                        panic!(
                            "an ordinary night landed no {dataset} at all — the refresh \
                             derives the catalog from it, so raw/ cannot rebuild the \
                             catalog without it"
                        )
                    });
                    assert!(
                        !landed.parts.is_empty(),
                        "{dataset} has a prefix but no parts in it"
                    );
                    assert!(landed.complete, "{dataset} landed an incomplete run");
                }
                Coverage::TailLagWindow(_why) => assert!(
                    manifest_for(&manifests, *dataset).is_none(),
                    "{dataset} landed on an ordinary night — expected nothing (the tail \
                     only fetches this when a new set has been published)"
                ),
            }
        }

        // 2. Everything asked for was stored — the call-site audit, made by
        //    comparison rather than by reading. A fetch added to the
        //    acquisition phase that skips `landing::fetch_bytes` shows up
        //    here as a served request with no part.
        let served = targets(
            upstream.requests()[before..].iter().cloned(),
            &upstream.base_url(),
        );
        let landed = targets(
            manifests
                .iter()
                .flat_map(|m| m.parts.iter().map(|p| p.url.clone())),
            &upstream.base_url(),
        );
        assert_eq!(
            served, landed,
            "every upstream response a refresh receives must be landed, exactly once"
        );

        // 3. Japanese TCGCSV (category 85) shares `source=tcgcsv` with
        //    English (category 3), so the only evidence it landed is in the
        //    URLs. Both categories, in the same prefixes.
        for dataset in [Dataset::Groups, Dataset::Products, Dataset::Prices] {
            let m = manifest_for(&manifests, dataset).expect("a tcgcsv prefix");
            let urls: Vec<&str> = m.parts.iter().map(|p| p.url.as_str()).collect();
            assert!(
                urls.iter().any(|u| u.contains("/3/")),
                "{dataset} landed nothing for English (category 3): {urls:?}"
            );
            assert!(
                urls.iter().any(|u| u.contains("/85/")),
                "{dataset} landed nothing for Pokémon Japan (category 85): {urls:?}"
            );
        }

        // 4. The bytes are on disk under the keys the manifests claim, not
        //    just in the manifests.
        let root = tmp.path().join("night-2");
        for m in &manifests {
            for part in &m.parts {
                assert!(
                    root.join(&part.key).is_file(),
                    "{} is in the manifest but not in the store",
                    part.key
                );
            }
        }
    }
}

/// Execute `pkdump data refresh` — fetch every upstream and LAND it.
///
/// ## It does not build the catalog any more (pd-lunn)
///
/// It used to: this function called [`pkdump_derive::derive`] and wrote
/// `shared.sqlite`, which meant the catalog had two builders. That is why
/// `pkdump-derive@<instance>.timer` stayed disabled everywhere — arming it
/// only did the same work a second time, from the bytes the first run had just
/// landed, overwriting a catalog that was already right.
///
/// Item 6 of the lake-as-source epic picks one. `pkdump-lake-derive shared` is
/// the builder; this command is the LANDING half and nothing else, which is
/// what the two units have claimed to be since item 5 shipped. The blocking
/// question — does a catalog replayed from `raw/` equal the one fetched
/// online — was answered against prod's own nightly partition on 2026-08-25:
/// row-identical across twenty tables, 12.6M price rows included.
///
/// So the shape here is now:
///
/// - the catalog is opened **read-only** (`open_shared_readonly`), and is
///   asked one question: which sets it already has. A read-only handle is why
///   "the refresh writes no catalog table" is a fact about the connection
///   rather than a claim about this function.
/// - landing is **required**, not a flag. See [`crate::landing::require`].
/// - the derivation happens hours later, in its own unit, from the partition
///   this run landed. `deploy/pkdump-derive.timer` is no longer optional on a
///   box that runs this: without it the catalog simply stops advancing.
///
/// ## Exit status (pd-nons)
///
/// | | |
/// | --- | --- |
/// | 0 | every upstream was acquired and landed |
/// | 2 | **partial**: the pokemontcg.io tail failed after exhausting its retries; the run continued and TCGCSV — the half a night cannot get back — was landed |
/// | 1 | the run failed |
///
/// 2 is a distinct status because the two outcomes want different answers: a
/// tail that fails one night costs a day's set list, a TCGCSV pull that fails
/// costs a day's prices permanently. It is deliberately **not** wired to
/// `SuccessExitStatus=` in `deploy/pkdump-refresh.service` — a set list that
/// silently stopped advancing is exactly the failure nothing else on the box
/// would report, so a partial run still reaches the wrapper's stall check. See
/// the unit for the argument.
fn refresh(args: RefreshCmdArgs) -> anyhow::Result<()> {
    let db_path = match args.common.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog READ-ONLY at {}", db_path.display());
    let conn = pkdump_db::open_shared_readonly(&db_path).map_err(|e| {
        anyhow::anyhow!(
            "{e}\n\nThe refresh reads the catalog to decide which sets are new; it does not \
             create one. If this box has never been set up, run `pkdump setup` first."
        )
    })?;

    // The run's clock, read ONCE — see `pkdump_derive::clock`. It picks the
    // ingest_date partition and it is recorded in every manifest, which is
    // what lets the offline derive stamp the same fetched_at / observed_at
    // into the same rows from these bytes.
    let clock = pkdump_derive::DeriveClock::now();

    // Resolved before anything is fetched: a landing zone that cannot be
    // opened stops the run at the start, not after an hour of requests whose
    // bytes then have nowhere to go.
    let landing = crate::landing::require(&clock)?;

    // Only the symbol phase reads this, and that phase is the deriving side's,
    // so nothing here opens it. Passed anyway rather than left to a default:
    // the field is not optional, and the catalog's own directory is the answer
    // every other command gives.
    let data_dir = db_path
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let report = pkdump_derive::land(
        &conn,
        &pkdump_derive::Options {
            clock,
            data_dir: &data_dir,
            landing: Some(landing),
            // The online side never replays. It has no way to: reading the
            // landing zone is `pkdump-lakehouse`'s job and this crate does
            // not depend on it.
            replay: None,
        },
    )?;

    if let Some(e) = report.tail_error {
        eprintln!("!! Refresh PARTIAL: nothing was derived, and the tail is short");
        eprintln!("!!   the pokemontcg.io tail failed after its retries: {e}");
        eprintln!(
            "!!   The run CONTINUED past it: TCGCSV groups, products and prices were fetched \
             and landed. Tonight's partition can still be derived; its set list will be as old \
             as the last partition that carried a whole one. Exit status 2."
        );
        // Not an `Err`: anyhow's main would print the error and exit 1, which
        // is the status a run that landed nothing carries. This one landed the
        // perishable half.
        drop(conn);
        std::process::exit(2);
    }

    println!("Refresh complete: landed, not derived. The catalog is built by pkdump-lake-derive.");
    Ok(())
}
