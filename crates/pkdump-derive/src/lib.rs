//! `pkdump-derive` — how `shared.sqlite` is built, and the only copy of it.
//!
//! This is the catalog derivation that used to be the body of
//! `pkdump-cli/src/data.rs::refresh`. It was **relocated**, not rewritten:
//! the phases below are the same phases in the same order calling the same
//! functions, and the diff that created this crate moved lines rather than
//! changing them. That is the point. The logic that decides what a card *is*
//! is substantial, correct and already tested, and porting it — to Python, or
//! to more Rust, which is worse because it looks like a refactor — buys
//! nothing and risks a long tail of differences in exactly the place where a
//! difference is invisible.
//!
//! ## Why it is a crate rather than a function in the CLI
//!
//! > Only lakehouse code reads `raw/`. The shared and tenant databases are
//! > derived from that, so whatever produces them is **also** lakehouse code.
//!
//! That rule is how the offline and online processes decouple onto different
//! machines. It rules out the obvious shape — a `--from-raw` flag on `pkdump
//! data refresh` — because that puts a raw reader inside `pkdump-cli`, on the
//! **online** side, which is the coupling the rule exists to break.
//!
//! So the derivation lives here, and there are two callers:
//!
//! | caller | side | where its bytes come from |
//! | --- | --- | --- |
//! | `pkdump data refresh` (`pkdump-cli`) | online | live upstreams, landed into `raw/` with `--land-raw` |
//! | `pkdump-lake-derive shared` (`pkdump-lakehouse`) | offline | `raw/`, replayed |
//!
//! **Nothing in this crate reads `raw/`.** It does not know the landing zone
//! can be read at all. It takes a [`pkdump_ingest::landing::Wire`], which is
//! either empty, or writing through to a landing zone, or answering from a
//! [`ReplaySource`](pkdump_ingest::landing::ReplaySource) somebody else
//! implemented. The crate that implements one over `raw/` is
//! `pkdump-lakehouse`, and it is a binary with no library, so no online
//! target can link it even by accident.
//!
//! ## What "the same rows twice" needs
//!
//! Two things, and both are inputs rather than ambient state:
//!
//! - **the clock** ([`DeriveClock`]) — read once by the landing side,
//!   recorded in the run's manifests, and read back by the deriving side. See
//!   `clock.rs`, which is the whole argument.
//! - **repo files** — `data/variants.json` and friends are compiled in
//!   (`include_str!`), so both sides of a comparison are running the same
//!   build's copy by construction. They are versioned in git already, which
//!   is why the design leaves them where they are rather than landing them.
//!
//! ## The one phase that cannot be replayed
//!
//! [`symbols::normalize_all_symbols`](pkdump_ingest::symbols) fetches PNGs
//! from `images.pokemontcg.io`, and images are **deliberately** not landed —
//! the retention arithmetic that justifies keeping `raw/` forever is for JSON
//! only. It still runs here, unchanged, because it is part of the
//! derivation; on a box with no egress every fetch fails, is counted in
//! `failed`, and the set keeps its upstream URL rather than a local one. That
//! is a real gap between an online refresh and an offline derive, it is
//! filed rather than papered over, and it is why this crate says so out loud
//! rather than leaving it to be discovered from a row count.

pub mod clock;

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use pkdump_ingest::landing::{ReplaySource, Wire};
use pkdump_ingest::pokemontcg::PokemonTcgClient;
use pkdump_ingest::tcgcsv::TcgcsvClient;
use pkdump_lake::RawLanding;

use pkdump_ingest::{coverage, japan, overrides, pokemon_tcg_data, symbols, tcgcsv};

pub use clock::DeriveClock;

/// Everything a derivation needs that is not the database.
pub struct Options<'a> {
    /// The instant the inputs were fetched. See [`DeriveClock`].
    pub clock: DeriveClock,
    /// Where set-symbol PNGs are cached — the catalog's data directory.
    pub data_dir: &'a Path,
    /// Land every upstream response before parsing it. `None` is the
    /// ordinary case; `Some` is `--land-raw`.
    pub landing: Option<Arc<RawLanding>>,
    /// Answer every upstream request from bytes already landed. `Some` is the
    /// offline derive; nothing online ever sets it.
    pub replay: Option<Arc<dyn ReplaySource>>,
}

impl Options<'_> {
    fn wire(&self) -> Wire {
        let mut wire = Wire::default();
        if let Some(landing) = &self.landing {
            wire = wire.landing_in(Arc::clone(landing));
        }
        if let Some(replay) = &self.replay {
            wire = wire.replaying(Arc::clone(replay));
        }
        wire
    }
}

/// What one derivation produced. Counts only — the phases print their own
/// progress as they go, because a run takes minutes and a summary at the end
/// is no use while it is still going.
#[derive(Debug, Default, Clone, Copy)]
pub struct Report {
    /// Sets the pokemontcg.io tail added.
    pub sets_added: usize,
    /// Rows written to `printings` by variant expansion.
    pub printings: usize,
    /// Rows materialised into `latest_prices`.
    pub latest_prices: usize,
    /// Set symbols that could not be normalised — see the crate docs. Non-zero
    /// on any box without egress to `images.pokemontcg.io`.
    pub symbols_failed: usize,
}

/// Derive the shared catalog: everything `pkdump data refresh` used to do
/// inline, in the same order.
///
/// The acquisition phase is bracketed so the landing zone's manifests are
/// written whichever way it ends — a run that dies partway must leave a
/// manifest that says so, not a short prefix that reads as whole. Everything
/// after that bracket is local derivation, and a failure there says nothing
/// about whether the raw bytes arrived.
pub fn derive(conn: &mut Connection, options: &Options<'_>) -> anyhow::Result<Report> {
    let mut report = Report::default();

    // 1. Reconcile the variants table from data/variants.json — runs
    //    first because it's purely local (no network) and idempotent.
    //    Putting it ahead of the network calls means a flaky upstream
    //    can't keep variants.json edits from landing on the next refresh.
    println!("Reconciling variants table from data/variants.json...");
    let n_variants = pkdump_db::variants::reconcile(conn)?;
    println!("  {n_variants} variant rows reconciled");

    // 1b. Reconcile (group, sub_type) → variant map from
    //     data/tcgcsv_sub_type_variants.json. Lives next to the
    //     variants seed and follows the same idempotent-reconcile
    //     pattern. Variant expansion (step 3 below) reads this back.
    println!("Reconciling tcgcsv_sub_type_variant_map...");
    let n_sub = pkdump_db::sub_type_map::reconcile(conn)?;
    println!("  {n_sub} (group, sub_type) → variant rows");

    // 1c. Reconcile the bundles registry from data/bundles.json. Drives
    //     the /api/sets dispatch for TTBB-style containers.
    println!("Reconciling bundles table from data/bundles.json...");
    let n_bundles = pkdump_db::bundles::reconcile(conn)?;
    println!("  {n_bundles} bundles registered");

    // 1d. Reconcile the search query language metadata from
    //     data/search_*.json (local + idempotent).
    println!("Reconciling search query metadata...");
    let sm = pkdump_db::search_meta::reconcile(conn)?;
    println!(
        "  {} keywords, {} rarities, {} flags",
        sm.keywords, sm.rarities, sm.flags
    );

    // 2. The acquisition phase — every step that reaches an upstream we
    //    keep bytes from, or replays one we kept. Bracketed, see the fn docs.
    let acquired = acquire(conn, options, &mut report);
    if let Some(landing) = &options.landing {
        finalize_landing(landing, acquired.as_ref().err())?;
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
    for d in pkdump_ingest::set_discovery::discover_new_sets(conn)? {
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
    let n_synth = tcgcsv::synthesize_cards_for_bridges(conn)?;
    println!("  {n_synth} cards synthesized");

    // Curated standalone promos (Ancient Mew, etc.) — see setup.rs step 5b.
    let n_promo = pkdump_ingest::standalone_promos::synthesize_standalone_promos(conn)?;
    println!("  {n_promo} standalone promos synthesized");

    // 5. Variant expansion. TCGCSV is authoritative for which printings a
    //    card has; the overlay still applies for cards TCGCSV can't model
    //    (cross-group stamped promos, etc.). Each printing carries its
    //    sub_type_name + tcgplayer_product_id so price queries stay a
    //    straight JOIN.
    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    report.printings = overrides::expand_all_printings(conn, &overlay)?;
    println!("  wrote {} printings", report.printings);

    // Report sets that mapped no printing to a TCGplayer product at all —
    // the shape `basep` sat in, unnoticed, for the catalog's whole life
    // (pd-0o5m). See `pkdump_ingest::coverage`.
    println!("Checking TCGplayer mapping coverage...");
    coverage::report_unmapped_sets(conn)?;

    // 5. Normalize set symbol glyphs for any new sets the tail fetch added.
    //    Existing rows already point at /sym/<set>.png and are skipped via
    //    the http-prefix gate in normalize_all_symbols.
    //
    //    The one phase a replay cannot supply — see the crate docs. It fetches
    //    images, and images are deliberately outside the landing zone.
    println!("Normalizing set symbol glyphs...");
    let s = symbols::normalize_all_symbols(conn, options.data_dir)?;
    report.symbols_failed = s.failed;
    println!(
        "  {} processed, {} cached, {} overrides, {} failed",
        s.processed, s.cached, s.overrides, s.failed
    );
    if s.failed > 0 && options.replay.is_some() {
        println!(
            "  NOTE: {} set symbol(s) could not be fetched. Symbols are IMAGES, and images are \
             deliberately not landed in raw/ — an offline derive cannot reproduce this phase \
             without egress to images.pokemontcg.io. The affected sets keep their upstream \
             symbol URL, which still renders.",
            s.failed
        );
    }

    // 6. Rebuild the materialized latest-prices table so the per-row
    //    market-price lookup on the collection/search/binder pages stays a
    //    point read rather than a GROUP BY over all of prices (vi37).
    println!("Refreshing materialized latest_prices...");
    report.latest_prices = pkdump_db::latest_prices::refresh_latest_prices(conn)?;
    println!("  {} latest-price rows materialized", report.latest_prices);

    //    Curated prices for catalog printings the feed does not price. Its
    //    rows FK into `printings`, so it runs after variant expansion; and it
    //    must land before anything values a collection from this catalog,
    //    which reads the same effective-price rule (pd-m4gw).
    let n_override = pkdump_db::catalog_prices::reconcile(conn)?;
    println!("  {n_override} curated catalog price overrides reconciled");

    // And that is the end of it. The derivation touches the SHARED catalog and
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
    // tests/refresh/tenant_bytes.sh is the gate: a refresh over a data
    // directory with real tenant databases in it must leave every one of them
    // byte-identical.
    Ok(report)
}

/// Everything in a derivation that reaches an upstream whose bytes we keep —
/// or replays bytes we kept.
///
/// Separated from the rest because acquiring and deriving are different jobs
/// with different failure meanings: a fetch that fails leaves the raw prefix
/// short and its manifest has to say so, while a variant expansion that fails
/// says nothing about the bytes, which are already landed and complete.
///
/// `symbols::normalize_all_symbols` also fetches, from
/// `images.pokemontcg.io`, and is deliberately *not* here: card art and set
/// symbols are excluded from the landing zone, because the retention
/// arithmetic that justifies keeping `raw/` forever is for JSON only.
fn acquire(
    conn: &mut Connection,
    options: &Options<'_>,
    report: &mut Report,
) -> anyhow::Result<()> {
    // 2. pokemontcg.io tail — pick up sets released since the last refresh.
    println!("Filling newest sets from pokemontcg.io...");
    report.sets_added = import_tail(conn, options)?;
    println!("  added {} set(s) not yet in the catalog", report.sets_added);

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
    let r = import_tcgcsv(conn, options)?;
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
        options.clock.fetched_at(),
        options.clock.observed_date(),
        options.wire(),
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
fn import_tail(conn: &mut Connection, options: &Options<'_>) -> anyhow::Result<usize> {
    let client = PokemonTcgClient::new()?.on_wire(options.wire());
    let now = options.clock.fetched_at();
    let mut added = 0;
    for set in client.fetch_sets()? {
        let exists: bool = conn
            .prepare("SELECT 1 FROM sets WHERE set_code = ?1 AND ptcgio_fetched_at IS NOT NULL")?
            .exists([&set.id])?;
        if exists {
            continue;
        }
        pokemon_tcg_data::upsert_set(conn, &set, now)?;
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
    options: &Options<'_>,
) -> anyhow::Result<(usize, usize, usize, usize)> {
    let client = TcgcsvClient::new()?.on_wire(options.wire());
    let now = options.clock.fetched_at();
    let observed = options.clock.observed_date();

    let groups = client.fetch_groups()?;
    let n_groups = tcgcsv::import_groups(conn, &groups, now)?;

    let mut n_sealed = 0;
    let mut n_cards = 0;
    let mut n_prices = 0;
    for group in &groups {
        let products = client.fetch_products(group.group_id)?;
        n_sealed += tcgcsv::import_sealed_products(conn, &products, now)?;
        n_cards += tcgcsv::import_products(conn, &products, now)?;
        let prices = client.fetch_prices(group.group_id)?;
        n_prices += tcgcsv::import_prices(conn, &prices, observed)?;
    }
    Ok((n_groups, n_sealed, n_cards, n_prices))
}

/// Write the run's manifests and report what landed.
///
/// `error` is the acquisition phase's failure, if it had one; every manifest
/// then records that the run stopped early. A manifest that cannot be
/// written is an error in its own right — an unwritten manifest is
/// indistinguishable from a run that never got that far — but it must not
/// mask the fetch failure that is the more useful diagnosis.
pub fn finalize_landing(
    landing: &Arc<RawLanding>,
    error: Option<&anyhow::Error>,
) -> anyhow::Result<()> {
    let text = error.map(|e| format!("{e:#}"));
    let outcome = landing.finalize(text.as_deref());

    for manifest in landing.manifests() {
        println!(
            "  raw: {}/{} — {} part(s), {} byte(s), {}",
            manifest.source,
            manifest.dataset,
            manifest.parts.len(),
            manifest.total_bytes(),
            if manifest.complete {
                "complete".to_string()
            } else {
                format!("INCOMPLETE ({} failure(s))", manifest.failures.len())
            }
        );
    }

    match outcome {
        Ok(()) => Ok(()),
        // The acquisition error is the one worth propagating; this one still
        // has to be said out loud rather than dropped.
        Err(e) if error.is_some() => {
            eprintln!("WARN: could not write the raw landing manifests: {e}");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
