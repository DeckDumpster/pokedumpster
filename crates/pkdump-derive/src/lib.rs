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
//! So the derivation lives here, and this crate is both halves of a night:
//!
//! | entry point | caller | side | what it does |
//! | --- | --- | --- | --- |
//! | [`land`] | `pkdump data refresh` (`pkdump-cli`) | online | fetches every upstream into `raw/`, and derives nothing |
//! | [`derive`] | `pkdump-lake-derive shared` (`pkdump-lakehouse`) | offline | replays one `raw/` partition into `shared.sqlite` |
//!
//! `pkdump data refresh` used to call [`derive`] too, so the catalog had two
//! builders and `pkdump-derive@<instance>.timer` shipped disabled everywhere —
//! arming it only did the same work a second time. pd-lunn deleted that half.
//! One catalog, one builder, and the two units are a pair rather than a
//! choice.
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
    /// Land every upstream response before parsing it. `Some` on every
    /// [`land`] run — that is what a landing run is — and `None` on an offline
    /// [`derive`], which is reading a partition rather than writing one.
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
#[derive(Debug, Default, Clone)]
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
    /// Why the pokemontcg.io tail did not run to the end, when it did not.
    ///
    /// `Some` is **partial**: everything else was acquired and every local
    /// phase ran, but the set list is as old as the last run that finished a
    /// tail. See [`acquire`] for why that is not an early return, and note
    /// that it is a `Report` field rather than an `Err` precisely so a caller
    /// has to look at it — the two callers answer it differently (`pkdump data
    /// refresh` exits 2, and its wrapper decides whether that pages; the
    /// offline derive refuses to record provenance).
    pub tail_error: Option<String>,
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

    // 1d. Report the search query language metadata seeded from
    //     data/search_*.json. The read-write open this connection came from
    //     reconciled it (connection.rs::converge), so it is read back rather
    //     than re-run (pd-dzu5).
    let sm = pkdump_db::search_meta::counts(conn)?;
    println!(
        "  {} keywords, {} rarities, {} flags",
        sm.keywords, sm.rarities, sm.flags
    );

    // 2. The acquisition phase — every step that reaches an upstream we
    //    keep bytes from, or replays one we kept. Bracketed, see the fn docs.
    //
    //    A failed pokemontcg.io tail is NOT in `acquired`: it is deferred
    //    into `report.tail_error` so the rest of the run still happens. It is
    //    deliberately not passed to `finalize_landing` either. That argument
    //    marks EVERY dataset incomplete, which is right for a run cut short —
    //    a `products` prefix with 200 of 450 groups in it must not read as
    //    whole — and wrong here: the tail is the only step that stopped, and
    //    every step after it ran to its end, so those prefixes are whole. The
    //    tail's own datasets still carry the failure `fetch_bytes` recorded,
    //    and `complete` is computed per dataset from that, so `sets` reads
    //    incomplete and `prices` reads complete. Which is the truth.
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
    report.printings = overrides::expand_all_printings(conn, &overlay, options.clock.fetched_at())?;
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

    // 7. The seeds whose rows FK into rows this run just created — curated
    //    catalog prices (into `printings`) and set-name aliases (into `sets`).
    //    `open_shared` reconciled them on the way in, when neither target
    //    existed for anything this run was about to ingest, so it wrote
    //    nothing for them; run here they land in the SAME run that created
    //    their targets, which is what makes one derive a fixed point rather
    //    than the first of two (pd-zg7o).
    //
    //    The curated prices must also land before anything values a collection
    //    from this catalog, which reads the same effective-price rule
    //    (pd-m4gw).
    let seeds = pkdump_db::reconcile_ingest_dependent_seeds(conn)?;
    println!(
        "  {} curated catalog price overrides, {} set aliases reconciled",
        seeds.catalog_prices, seeds.set_aliases
    );

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

    // Said once at the point of failure and once here, for the same reason
    // the offline job restates its replay misses: everything between the two
    // is minutes of progress lines, and a warning that scrolled past an hour
    // ago is a warning nobody read.
    if let Some(e) = &report.tail_error {
        eprintln!("!! PARTIAL DERIVATION: the pokemontcg.io tail did not complete: {e}");
    }
    Ok(report)
}

/// Fetch every upstream a derivation reads, land the bytes, and derive
/// **nothing** — the landing half of [`derive`] on its own (pd-lunn).
///
/// This is what `pkdump data refresh` runs now. The catalog it is handed is
/// opened read-only and asked exactly one question ([`missing_sets`]); no row
/// of it is written by this function or by anything it calls, and the rows are
/// written some hours later by `pkdump-lake-derive shared`, replaying the
/// partition this run just landed. One catalog, one builder.
///
/// ## Why a second function and not a flag on `derive`
///
/// Because the two have different *shapes*, not different settings. `derive`
/// takes `&mut Connection` and writes a catalog; this takes `&Connection` and
/// cannot. A boolean inside `derive` would leave every phase below the
/// acquisition reachable with a flag set wrong, and the claim "the refresh
/// writes no catalog table" would be a thing to review rather than a thing the
/// type says.
///
/// ## What makes the coverage claim hold
///
/// A landing run is only useful if the partition it leaves answers every URL
/// the later derive asks for — item 4 removed the upstream fallback, so a URL
/// that is missing is a refusal, not a quiet re-fetch. Three things keep the
/// two in step, and none of them is vigilance:
///
/// - the one catalog-dependent choice is [`missing_sets`], called by both;
/// - every other URL comes off the wire (the two TCGCSV group lists) or is a
///   fixed endpoint, so there is nothing to disagree about;
/// - `crates/pkdump-lakehouse/tests/row_identical.rs` derives a catalog from a
///   partition landed by THIS function and diffs it, row for row, against one
///   `derive` built from the same upstream. A URL this function stops asking
///   for fails that gate rather than tomorrow's timer.
///
/// The tail is allowed to fail without ending the run, for the reason
/// [`acquire`] gives: TCGCSV is the half a night cannot get back. A caller
/// reads [`Report::tail_error`] and decides — `pkdump data refresh` exits 2.
///
/// `symbols::normalize_all_symbols` is not here, and its absence is not an
/// omission: it fetches images, images are deliberately not landed, and it is
/// a derivation phase rather than an acquisition one. The offline derive runs
/// it, live, against `images.pokemontcg.io`.
pub fn land(conn: &Connection, options: &Options<'_>) -> anyhow::Result<Report> {
    let mut report = Report::default();

    // The same bracket `derive` puts round its acquisition, for the same
    // reason: a run that dies partway must leave a manifest that says so
    // rather than a short prefix that reads as whole.
    let acquired = land_acquisition(conn, options, &mut report);
    if let Some(landing) = &options.landing {
        finalize_landing(landing, acquired.as_ref().err())?;
    }
    acquired?;

    if let Some(e) = &report.tail_error {
        eprintln!("!! PARTIAL LANDING: the pokemontcg.io tail did not complete: {e}");
    }
    Ok(report)
}

/// [`acquire`]'s fetches, without the imports. Kept immediately beside it so
/// the two orders are read together rather than remembered apart.
fn land_acquisition(
    conn: &Connection,
    options: &Options<'_>,
    report: &mut Report,
) -> anyhow::Result<()> {
    println!("Landing the newest sets from pokemontcg.io...");
    match land_tail(conn, options) {
        Ok(added) => {
            report.sets_added = added;
            println!("  landed {added} set(s) not yet in the catalog");
        }
        Err(e) => {
            let text = format!("{e:#}");
            eprintln!("!! the pokemontcg.io tail FAILED after exhausting its retries: {text}");
            eprintln!(
                "!! The catalog's set list will be as old as the last derive that had a whole \
                 tail to replay. The run CONTINUES: TCGCSV is the half a night cannot lose and \
                 it has not been fetched yet (pd-nons). This run is PARTIAL."
            );
            report.tail_error = Some(text);
        }
    }

    println!("Landing TCGCSV groups, products, prices...");
    let groups = land_tcgcsv(options)?;
    println!("  {groups} group(s) walked");

    println!("Landing the Pokémon Japan catalog (TCGCSV category 85)...");
    let jp = japan::land_all(options.wire())?;
    println!("  {jp} group(s) walked");

    Ok(())
}

/// Fetch everything [`import_tcgcsv`] would fetch, and import none of it.
fn land_tcgcsv(options: &Options<'_>) -> anyhow::Result<usize> {
    let client = TcgcsvClient::new()?.on_wire(options.wire());
    let groups = client.fetch_groups()?;
    for group in &groups {
        client.fetch_products(group.group_id)?;
        client.fetch_prices(group.group_id)?;
    }
    Ok(groups.len())
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
///
/// ## The tail may fail without ending the run (pd-nons)
///
/// On 2026-08-11 `api.pokemontcg.io` answered 500 or 502 to roughly 45% of
/// requests. One landed on `/v2/sets?page=1`, the error propagated, and
/// `pkdump data refresh` was over in its first second — before TCGCSV was
/// reached, so **no prices were imported at all**. A day's prices cannot be
/// re-fetched later; a day's set list can, because tomorrow's copy of it is a
/// superset of tonight's.
///
/// So the tail's error is *carried* — into [`Report::tail_error`], out to a
/// caller that reports it — instead of thrown. Nothing is swallowed and
/// nothing is defaulted: the sets that did not arrive are absent rather than
/// invented, the failure is printed where it happens and again at the end of
/// the run, and the run reports itself partial. It is the only step here
/// allowed to do that. A TCGCSV failure still ends the acquisition, because
/// there is no later run that can recover what it would have fetched.
///
/// ## Why the tail is still FIRST, which looks backwards
///
/// The obvious companion change — fetch the perishable dataset first, so a
/// slow tail cannot eat the unit's time budget before prices are in — was
/// tried, and reverted. It breaks the catalog.
///
/// `tcgcsv::import_groups` resolves `tcgplayer_groups.set_code` by matching
/// each group against the `sets` rows **already in the database**. Run it
/// before the tail on a catalog that does not have those rows yet and every
/// link comes out NULL, to be filled in by the *next* derivation — which is
/// exactly the fixed point `crates/pkdump-lakehouse/tests/row_identical.rs`
/// holds the derivation to. An offline rebuild from `raw/` starts from an
/// empty catalog every time, so the reordering did not cost "one night of
/// linking on a newly published set": it made a replayed catalog differ from
/// the online one it exists to reproduce, in `tcgplayer_groups`,
/// `sealed_products` and `printings`. That gate caught it on the first run.
///
/// The reordering was also the weaker half of the fix. What was observed is
/// an *error*, and an error no longer ends the run. The exposure reordering
/// would additionally close is a tail that HANGS — and that one is already
/// bounded: a 30s request timeout times a retry budget of 4, against the
/// unit's `TimeoutStartSec=1800`.
fn acquire(
    conn: &mut Connection,
    options: &Options<'_>,
    report: &mut Report,
) -> anyhow::Result<()> {
    // 2. pokemontcg.io tail — pick up sets released since the last refresh.
    //    The one step here allowed to fail without ending the run; see the fn
    //    docs. Its retries are already spent by the time it returns an error
    //    (`pkdump_ingest::retry`).
    println!("Filling newest sets from pokemontcg.io...");
    match import_tail(conn, options) {
        Ok(added) => {
            report.sets_added = added;
            println!("  added {added} set(s) not yet in the catalog");
        }
        Err(e) => {
            let text = format!("{e:#}");
            eprintln!("!! the pokemontcg.io tail FAILED after exhausting its retries: {text}");
            eprintln!(
                "!! The catalog's set list will be as old as the last run that finished one. \
                 The run CONTINUES: TCGCSV is the half a night cannot lose and it has not been \
                 fetched yet (pd-nons). This run is PARTIAL."
            );
            report.tail_error = Some(text);
        }
    }

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

    // 2. TCGCSV groups, sealed products, single-card products, prices — raw
    //    ingest of everything TCGCSV publishes. Variant expansion in step 3
    //    reads this back out to determine which printings actually exist for
    //    each card. THE dataset a lost night cannot get back: a price is a
    //    fact about one day and there is no asking for it later.
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

/// The pokemontcg.io sets this catalog does not have yet.
///
/// A set row that exists but carries no `ptcgio_fetched_at` was
/// synthesized locally — from a bridge entry, or by TCGCSV set discovery
/// while upstream was still behind. Those count as missing: importing them
/// is exactly how the real cards supersede the synthesized stubs the day
/// pokemontcg.io publishes the set.
///
/// **The only decision in the whole acquisition that depends on what the
/// catalog already holds**, which is why it is one function that both callers
/// share rather than a predicate written twice. [`import_tail`] imports exactly
/// these sets and [`land_tail`] lands exactly these sets' cards, so the URLs a
/// landing run puts in `raw/` are the URLs the derive that replays it will ask
/// for. Everything else either comes off the wire (the TCGCSV group lists) or
/// is a fixed endpoint.
fn missing_sets(
    conn: &Connection,
    client: &PokemonTcgClient,
) -> anyhow::Result<Vec<pkdump_ingest::pokemontcg::PokemonTcgSet>> {
    let mut missing = Vec::new();
    for set in client.fetch_sets()? {
        let exists: bool = conn
            .prepare("SELECT 1 FROM sets WHERE set_code = ?1 AND ptcgio_fetched_at IS NOT NULL")?
            .exists([&set.id])?;
        if !exists {
            missing.push(set);
        }
    }
    Ok(missing)
}

/// Fetch the pokemontcg.io set list and import any set the catalog lacks.
fn import_tail(conn: &mut Connection, options: &Options<'_>) -> anyhow::Result<usize> {
    let client = PokemonTcgClient::new()?.on_wire(options.wire());
    let now = options.clock.fetched_at();
    let sets = missing_sets(conn, &client)?;
    for set in &sets {
        pokemon_tcg_data::upsert_set(conn, set, now)?;
        for card in client.fetch_cards_for_set(&set.id)? {
            pokemon_tcg_data::upsert_card(conn, &card, &set.id)?;
        }
    }
    Ok(sets.len())
}

/// Fetch everything [`import_tail`] would fetch, and import none of it.
fn land_tail(conn: &Connection, options: &Options<'_>) -> anyhow::Result<usize> {
    let client = PokemonTcgClient::new()?.on_wire(options.wire());
    let sets = missing_sets(conn, &client)?;
    for set in &sets {
        client.fetch_cards_for_set(&set.id)?;
    }
    Ok(sets.len())
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
