//! `pkdump data refresh` — nightly incremental catalog update.
//!
//! Unlike `pkdump setup`, this skips the `pokemon-tcg-data` bulk import. It
//! re-fetches the tail of newest sets from the pokemontcg.io API, re-imports
//! TCGCSV groups, sealed products, and prices, then re-applies the dirty-data
//! overrides. Each run appends a fresh price snapshot (a new `observed_at`)
//! to the `prices` table — the source for a future price-history chart.

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;

use pkdump_lake::RawLanding;

use pkdump_ingest::pokemontcg::PokemonTcgClient;
use pkdump_ingest::tcgcsv::TcgcsvClient;
use pkdump_ingest::{coverage, japan, overrides, pokemon_tcg_data, symbols, tcgcsv};

/// The `pkdump data` subcommand group.
#[derive(clap::Args)]
pub struct DataArgs {
    #[command(subcommand)]
    command: DataCommand,
}

#[derive(clap::Subcommand)]
enum DataCommand {
    /// Incrementally refresh the shared catalog: newest sets, prices, and
    /// dirty-data overrides.
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
#[derive(clap::Args)]
pub struct RefreshCmdArgs {
    #[command(flatten)]
    common: RefreshArgs,

    /// Land every upstream response in the raw landing zone before parsing
    /// it, under `raw/source=.../ingest_date=.../run=<ULID>/`.
    ///
    /// Requires ~/.config/pkdump/lake.env to name the bucket; the command
    /// refuses to start without it rather than landing nothing quietly.
    /// Card art and set symbols are never landed. Also settable as
    /// PKDUMP_LAND_RAW=1, which is how the containerised nightly refresh
    /// turns it on.
    #[arg(long)]
    land_raw: bool,
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
    let printings = overrides::expand_all_printings(&mut conn, &overlay)?;
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

/// Execute `pkdump data refresh`.
fn refresh(args: RefreshCmdArgs) -> anyhow::Result<()> {
    let db_path = match args.common.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    // Resolved before anything is fetched: a landing zone that was asked for
    // and is not configured should stop the run at the start, not after an
    // hour of requests whose bytes then have nowhere to go.
    let landing = crate::landing::open(
        args.land_raw,
        &chrono::Utc::now().format("%Y-%m-%d").to_string(),
    )?;

    // 1. Reconcile the variants table from data/variants.json — runs
    //    first because it's purely local (no network) and idempotent.
    //    Putting it ahead of the network calls means a flaky upstream
    //    can't keep variants.json edits from landing on the next refresh.
    println!("Reconciling variants table from data/variants.json...");
    let n_variants = pkdump_db::variants::reconcile(&mut conn)?;
    println!("  {n_variants} variant rows reconciled");

    // 1b. Reconcile (group, sub_type) → variant map from
    //     data/tcgcsv_sub_type_variants.json. Lives next to the
    //     variants seed and follows the same idempotent-reconcile
    //     pattern. Variant expansion (step 3 below) reads this back.
    println!("Reconciling tcgcsv_sub_type_variant_map...");
    let n_sub = pkdump_db::sub_type_map::reconcile(&mut conn)?;
    println!("  {n_sub} (group, sub_type) → variant rows");

    // 1c. Reconcile the bundles registry from data/bundles.json. Drives
    //     the /api/sets dispatch for TTBB-style containers.
    println!("Reconciling bundles table from data/bundles.json...");
    let n_bundles = pkdump_db::bundles::reconcile(&mut conn)?;
    println!("  {n_bundles} bundles registered");

    // 1d. Reconcile the search query language metadata from
    //     data/search_*.json (local + idempotent).
    println!("Reconciling search query metadata...");
    let sm = pkdump_db::search_meta::reconcile(&mut conn)?;
    println!(
        "  {} keywords, {} rarities, {} flags",
        sm.keywords, sm.rarities, sm.flags
    );

    // 2. The acquisition phase — every step that reaches an upstream we
    //    keep bytes from. Bracketed so the landing zone's manifests are
    //    written whichever way it ends: a run that dies partway must leave
    //    a manifest that says so, not a short prefix that reads as whole.
    //    Everything after this point is local derivation, and a failure
    //    there says nothing about whether the raw bytes arrived.
    let acquired = acquire(&mut conn, landing.as_ref());
    if let Some(landing) = &landing {
        crate::landing::finalize_landing(landing, acquired.as_ref().err())?;
    }
    acquired?;

    // 2c. Auto-discover sets TCGCSV has published and pokemontcg.io
    //     hasn't — a numbered expansion group that bridges to nothing
    //     becomes a set + cards on its own, no hand-authored bridge and
    //     no waiting on upstream (pd-558b1e4f). Reads the products just
    //     imported, so it has to run after import_tcgcsv — and after the
    //     Japanese import, which bridges every category-85 group and so
    //     keeps them out of the unbridged pool discovery works from.
    println!("Discovering new sets from unbridged TCGCSV groups...");
    for d in pkdump_ingest::set_discovery::discover_new_sets(&mut conn)? {
        println!(
            "  {} ({}) — {} from group {}, {} cards",
            d.set_code, d.series, d.name, d.group_id, d.cards
        );
    }

    // 3. Synthesize card rows for bridged TCGCSV groups whose upstream
    //    pokemontcg.io entry doesn't exist yet (e.g. MEP). Idempotent
    //    INSERT OR IGNORE — once pokemontcg.io publishes the real set,
    //    upserts from import_tail win and synthesized stubs stand down.
    println!("Synthesizing cards for bridged groups...");
    let n_synth = tcgcsv::synthesize_cards_for_bridges(&mut conn)?;
    println!("  {n_synth} cards synthesized");

    // Curated standalone promos (Ancient Mew, etc.) — see setup.rs step 5b.
    let n_promo = pkdump_ingest::standalone_promos::synthesize_standalone_promos(&mut conn)?;
    println!("  {n_promo} standalone promos synthesized");

    // 5. Variant expansion. TCGCSV is authoritative for which printings a
    //    card has; the overlay still applies for cards TCGCSV can't model
    //    (cross-group stamped promos, etc.). Each printing carries its
    //    sub_type_name + tcgplayer_product_id so price queries stay a
    //    straight JOIN.
    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    let printings = overrides::expand_all_printings(&mut conn, &overlay)?;
    println!("  wrote {printings} printings");

    // Report sets that mapped no printing to a TCGplayer product at all —
    // the shape `basep` sat in, unnoticed, for the catalog's whole life
    // (pd-0o5m). See `pkdump_ingest::coverage`.
    println!("Checking TCGplayer mapping coverage...");
    coverage::report_unmapped_sets(&conn)?;

    // 5. Normalize set symbol glyphs for any new sets the tail fetch added.
    //    Existing rows already point at /sym/<set>.png and are skipped via
    //    the http-prefix gate in normalize_all_symbols.
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

    // 6. Rebuild the materialized latest-prices table so the per-row
    //    market-price lookup on the collection/search/binder pages stays a
    //    point read rather than a GROUP BY over all of prices (vi37).
    println!("Refreshing materialized latest_prices...");
    let n_latest = pkdump_db::latest_prices::refresh_latest_prices(&conn)?;
    println!("  {n_latest} latest-price rows materialized");

    //    Curated prices for catalog printings the feed does not price. Its
    //    rows FK into `printings`, so it runs after variant expansion; and it
    //    must land before anything values a collection from this catalog,
    //    which reads the same effective-price rule (pd-m4gw).
    let n_override = pkdump_db::catalog_prices::reconcile(&mut conn)?;
    println!("  {n_override} curated catalog price overrides reconciled");

    // And that is the end of it. The refresh touches the SHARED catalog and
    // nothing else — no tenant database is opened, let alone written.
    //
    // It used to end with a step 7 that snapshotted today's collection value
    // into `$PKDUMP_USER`'s database. One tenant's, out of however many the
    // registry holds: no loop, no error, and everybody else silently without a
    // value chart (pd-s5yn). Looping here was considered and rejected — the
    // catalog refresh is not the place that knows about tenants, and a refresh
    // that half-writes N collections fails in a worse way than one that writes
    // none. `lake/src/pkdump_lake/value_snapshots.py` owns that job now: it
    // walks the registry, values every tenant from `catalog.prices` at a pinned
    // Nessie commit, and reports per-tenant (pd-ruwh).
    //
    // `value_history::snapshot_today` itself stays — it is the reference the
    // transform is diffed against in tests/lake/value_snapshots.sh. It is no
    // longer called from any command: `pkdump data backfill-value-history`
    // goes through `value_history::backfill`, which reconstructs each date
    // with `backfill_one_date`.
    //
    // tests/refresh/tenant_bytes.sh is the gate: a refresh over a data
    // directory with real tenant databases in it must leave every one of them
    // byte-identical.
    println!("Refresh complete: {}", db_path.display());
    Ok(())
}

/// Everything in a refresh that reaches an upstream whose bytes we keep.
///
/// Separated from the rest of `refresh` because acquiring and deriving are
/// different jobs with different failure meanings: a fetch that fails leaves
/// the raw prefix short and its manifest has to say so, while a variant
/// expansion that fails says nothing about the bytes, which are already
/// landed and complete.
///
/// `symbols::normalize_all_symbols` also fetches, from
/// `images.pokemontcg.io`, and is deliberately *not* here: card art and set
/// symbols are excluded from the landing zone, because the retention
/// arithmetic that justifies keeping `raw/` forever is for JSON only.
fn acquire(conn: &mut Connection, landing: Option<&Arc<RawLanding>>) -> anyhow::Result<()> {
    // 2. pokemontcg.io tail — pick up sets released since the last refresh.
    println!("Filling newest sets from pokemontcg.io...");
    let added = import_tail(conn, landing)?;
    println!("  added {added} set(s) not yet in the catalog");

    // 2b. Re-apply the upstream-correction registry to rows already in the
    //     catalog. `upsert_card` above only corrects the sets import_tail
    //     just added — a correction registered after a card landed would
    //     otherwise never reach its row. Runs before variant expansion so
    //     downstream phases see the corrected numbers.
    println!("Re-applying upstream card corrections...");
    let healed = pokemon_tcg_data::apply_corrections_to_db(conn)?;
    for h in &healed {
        println!(
            "  {} number {} -> {}",
            h.card_id, h.current_number, h.corrected_number
        );
    }
    println!("  {} row(s) healed", healed.len());

    // 2. TCGCSV groups, sealed products, single-card products, prices —
    //    raw ingest of everything TCGCSV publishes. Variant expansion in
    //    step 3 reads this back out to determine which printings actually
    //    exist for each card.
    println!("Importing TCGCSV groups, products, prices...");
    let r = import_tcgcsv(conn, landing)?;
    println!(
        "  {} groups, {} sealed products, {} card products, {} price rows",
        r.0, r.1, r.2, r.3
    );

    // 2b. Pokémon Japan (TCGCSV categoryId 85) — sets and cards are
    //     synthesized straight from TCGCSV, there being no pokemontcg.io
    //     counterpart. Runs after the English pass so the two never
    //     contend for a set_code. See `pkdump_ingest::japan`.
    println!("Importing the Pokémon Japan catalog (TCGCSV category 85)...");
    let j = japan::import_all(
        conn,
        &chrono::Utc::now().to_rfc3339(),
        &chrono::Utc::now().format("%Y-%m-%d").to_string(),
        landing.cloned(),
    )?;
    println!(
        "  {} groups, {} cards, {} card products, {} sealed products, {} price rows",
        j.groups, j.cards, j.card_products, j.sealed_products, j.price_rows
    );

    Ok(())
}

/// Fetch the pokemontcg.io set list and import any set the catalog lacks.
///
/// A set row that exists but carries no `ptcgio_fetched_at` was
/// synthesized locally — from a bridge entry, or by TCGCSV set discovery
/// while upstream was still behind. Those count as missing: importing them
/// is exactly how the real cards supersede the synthesized stubs the day
/// pokemontcg.io publishes the set.
///
/// **With landing on, the cards of the sets it skips are still fetched.**
/// Importing is incremental — a set already in the catalog needs nothing —
/// but landing is not: the lake's premise is that `raw/` on its own can
/// rebuild what the catalog holds, and a night that lands only the cards of
/// sets published since yesterday lands nothing at all on almost every night
/// (pd-v1ca). Those responses are landed and dropped, so the rows this
/// function writes are identical either way; what changes is only what `raw/`
/// contains afterwards. `setup` needs no equivalent — its `pokemon-tcg-data`
/// tarball is one object carrying every set and card, landed as
/// `dataset=bulk`.
fn import_tail(conn: &mut Connection, landing: Option<&Arc<RawLanding>>) -> anyhow::Result<usize> {
    let client = crate::landing::with_landing(
        PokemonTcgClient::new()?,
        landing,
        PokemonTcgClient::landing_in,
    );
    let now = chrono::Utc::now().to_rfc3339();
    let mut added = 0;
    for set in client.fetch_sets()? {
        let exists: bool = conn
            .prepare("SELECT 1 FROM sets WHERE set_code = ?1 AND ptcgio_fetched_at IS NOT NULL")?
            .exists([&set.id])?;
        if exists {
            if landing.is_some() {
                // Landed on the way past by the client itself; the parsed
                // cards are of no use here, the rows already exist.
                client.fetch_cards_for_set(&set.id)?;
            }
            continue;
        }
        pokemon_tcg_data::upsert_set(conn, &set, &now)?;
        for card in client.fetch_cards_for_set(&set.id)? {
            pokemon_tcg_data::upsert_card(conn, &card, &set.id)?;
        }
        added += 1;
    }
    Ok(added)
}

/// Import every TCGCSV group: sealed products, single-card products
/// (persisted to `tcgcsv_products` for variant expansion to read), and a
/// fresh price snapshot. Returns (groups, sealed products, card products,
/// price rows).
fn import_tcgcsv(
    conn: &mut Connection,
    landing: Option<&Arc<RawLanding>>,
) -> anyhow::Result<(usize, usize, usize, usize)> {
    let client =
        crate::landing::with_landing(TcgcsvClient::new()?, landing, TcgcsvClient::landing_in);
    let now = chrono::Utc::now().to_rfc3339();
    let observed = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let groups = client.fetch_groups()?;
    let n_groups = tcgcsv::import_groups(conn, &groups, &now)?;

    let mut n_sealed = 0;
    let mut n_cards = 0;
    let mut n_prices = 0;
    for group in &groups {
        let products = client.fetch_products(group.group_id)?;
        n_sealed += tcgcsv::import_sealed_products(conn, &products, &now)?;
        n_cards += tcgcsv::import_products(conn, &products, &now)?;
        let prices = client.fetch_prices(group.group_id)?;
        n_prices += tcgcsv::import_prices(conn, &prices, &observed)?;
    }
    Ok((n_groups, n_sealed, n_cards, n_prices))
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
    use std::sync::{Mutex, MutexGuard};

    use pkdump_ingest::test_upstream::{FakeUpstream, Reply};
    use pkdump_ingest::upstream::{ENV_POKEMONTCG_BASE_URL, ENV_TCGCSV_BASE_URL};
    use pkdump_lake::{Dataset, DirStore, Manifest, RawLanding};

    use super::*;

    const INGEST_DATE: &str = "2026-08-12";

    /// Where a dataset's bytes come from, for the audit below.
    enum Coverage {
        /// Every landing-enabled refresh lands it, every night.
        Refresh,
        /// A `pkdump setup` input the refresh deliberately never fetches.
        /// Carries the reason, so an exemption is always a stated one.
        SetupOnly(&'static str),
    }

    /// The match is exhaustive on purpose: a new [`Dataset`] does not compile
    /// until somebody says whether a refresh has to land it. That is what
    /// makes this a gate against the *next* gap rather than a fix for this
    /// one.
    fn coverage(dataset: Dataset) -> Coverage {
        match dataset {
            Dataset::Sets
            | Dataset::Cards
            | Dataset::Groups
            | Dataset::Products
            | Dataset::Prices => Coverage::Refresh,
            Dataset::Bulk => Coverage::SetupOnly(
                "the pokemon-tcg-data tarball is a `pkdump setup` input — the refresh \
                 skips the bulk import by design (see the module docs)",
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
    /// pokemontcg.io one is `/v2`, exactly as the real hosts are shaped.
    fn route(target: &str, _n: usize) -> Reply {
        match target.split('?').next().unwrap_or(target) {
            "/3/groups" => Reply::ok(ENGLISH_GROUPS),
            "/85/groups" => Reply::ok(JAPAN_GROUPS),
            "/v2/sets" => Reply::ok(SETS),
            "/v2/cards" => Reply::ok(CARDS),
            p if p.ends_with("/products") || p.ends_with("/prices") => Reply::ok(EMPTY),
            other => Reply {
                status: 404,
                body: format!(
                    r#"{{"error":"the acquisition phase asked for {other}, which this \
                        fixture does not model — a new upstream call needs a route here \
                        AND a landed dataset"}}"#
                ),
            },
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
            // and this is the only test in the binary that touches them.
            unsafe {
                std::env::set_var(ENV_TCGCSV_BASE_URL, base);
                std::env::set_var(ENV_POKEMONTCG_BASE_URL, format!("{base}/v2"));
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
            }
        }
    }

    /// One landing-enabled acquisition against `db`, landing into `dir`.
    /// Returns the manifests as finalized.
    fn acquire_landing(db: &Path, dir: &Path) -> Vec<Manifest> {
        let mut conn = pkdump_db::open_shared(db).expect("open the catalog");
        let landing = Arc::new(RawLanding::new(Box::new(DirStore::new(dir)), INGEST_DATE));
        let outcome = acquire(&mut conn, Some(&landing));
        crate::landing::finalize_landing(&landing, outcome.as_ref().err())
            .expect("write the manifests");
        outcome.expect("the acquisition phase");
        landing.manifests()
    }

    /// One acquisition with landing off — the ordinary production path.
    fn acquire_plain(db: &Path) {
        let mut conn = pkdump_db::open_shared(db).expect("open the catalog");
        acquire(&mut conn, None).expect("the acquisition phase");
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
        acquire_landing(&db, &tmp.path().join("night-1"));

        // Night two: every set is already in the catalog. The ordinary night,
        // and the one every night after the first looks like.
        let before = upstream.requests().len();
        let manifests = acquire_landing(&db, &tmp.path().join("night-2"));

        // 1. Every dataset a refresh is responsible for, walked from the enum
        //    rather than from a list written here.
        for dataset in Dataset::ALL {
            match coverage(dataset) {
                Coverage::Refresh => {
                    let landed = manifest_for(&manifests, dataset).unwrap_or_else(|| {
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
                Coverage::SetupOnly(why) => assert!(
                    manifest_for(&manifests, dataset).is_none(),
                    "a refresh landed {dataset}, which it is not supposed to fetch: {why}"
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

    /// Landing is a tee: turning it on changes what is *stored*, never what
    /// is imported. The cards sweep an ordinary night now makes is fetched
    /// and dropped — the rows it would write are already in the catalog —
    /// so a landed refresh and an unlanded one leave the same database.
    #[test]
    fn landing_changes_what_is_stored_and_not_what_is_imported() {
        let upstream = FakeUpstream::start(route);
        let _origins = Origins::point_at(&upstream.base_url());
        let tmp = tempfile::tempdir().unwrap();

        let dump = |db: &Path| -> Vec<(String, String, i64)> {
            let conn = pkdump_db::open_shared(db).unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT 'sets', set_code, 0 FROM sets \
                     UNION ALL SELECT 'cards', card_id, 0 FROM cards \
                     UNION ALL SELECT 'groups', name, group_id FROM tcgplayer_groups \
                     ORDER BY 1, 2, 3",
                )
                .unwrap();
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };

        // Two catalogs, each acquired twice — first night and ordinary
        // night — one with landing on and one with it off.
        let landed_db = tmp.path().join("landed.sqlite");
        acquire_landing(&landed_db, &tmp.path().join("landed-1"));
        let before_landed = upstream.requests().len();
        acquire_landing(&landed_db, &tmp.path().join("landed-2"));
        let landed_requests = upstream.requests().len() - before_landed;

        let plain_db = tmp.path().join("plain.sqlite");
        acquire_plain(&plain_db);
        let before_plain = upstream.requests().len();
        acquire_plain(&plain_db);
        let plain_requests = upstream.requests().len() - before_plain;

        assert_eq!(
            dump(&landed_db),
            dump(&plain_db),
            "landing must not change a single row the refresh imports"
        );
        assert_eq!(
            landed_requests,
            plain_requests + 1,
            "an ordinary landed night fetches exactly one thing an unlanded one does \
             not: the cards of the set it already has"
        );
    }
}
