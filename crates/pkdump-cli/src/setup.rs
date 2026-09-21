//! `pkdump setup` — build the shared catalog database from upstream sources.
//!
//! Pipeline (PLAN.md §4.1):
//!   1. bulk import the `pokemon-tcg-data` repo (sets + cards),
//!   2. fill the tail of newest sets from the pokemontcg.io API,
//!   3. import TCGCSV groups, sealed products, and prices — first for
//!      English (category 3), then for the Pokémon Japan catalog
//!      (category 85), whose sets and cards TCGCSV alone supplies,
//!   4. run three-layer variant expansion into `printings`.
//!
//! Steps 2 and 3 hit the network; `--skip-tail` / `--skip-prices` /
//! `--skip-japan` turn them off, and `--from-dir` imports a local repo
//! checkout instead of downloading. With those, setup runs fully offline.

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::Connection;

use pkdump_derive::DeriveClock;
use pkdump_lake::RawLanding;

use pkdump_ingest::pokemontcg::PokemonTcgClient;
use pkdump_ingest::tcgcsv::TcgcsvClient;
use pkdump_ingest::{
    coverage, japan, overrides, pokemon_tcg_data, standalone_promos, symbols, tcgcsv,
};

/// Arguments for `pkdump setup`.
#[derive(clap::Args)]
pub struct SetupArgs {
    /// Import the catalog from a local pokemon-tcg-data checkout instead of
    /// downloading the repo tarball.
    #[arg(long, value_name = "DIR")]
    from_dir: Option<PathBuf>,

    /// Skip the pokemontcg.io tail fetch (sets newer than the repo).
    #[arg(long)]
    skip_tail: bool,

    /// Skip the TCGCSV sealed-product and price import.
    #[arg(long)]
    skip_prices: bool,

    /// Skip the Pokémon Japan catalog (TCGCSV categoryId 85). Roughly
    /// doubles the TCGCSV pass when left on — 450 extra groups.
    #[arg(long)]
    skip_japan: bool,

    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,

    /// Land every upstream response in the raw landing zone before parsing
    /// it, under `raw/source=.../ingest_date=.../run=<ULID>/`.
    ///
    /// Requires ~/.config/pkdump/lake.env to name the bucket; the command
    /// refuses to start without it rather than landing nothing quietly.
    /// Card art and set symbols are never landed. Also settable as
    /// PKDUMP_LAND_RAW=1.
    #[arg(long)]
    land_raw: bool,
}

/// Execute `pkdump setup`.
pub fn run(args: SetupArgs) -> anyhow::Result<()> {
    let db_path = match args.db.clone() {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    println!("Opening shared catalog at {}", db_path.display());
    let mut conn = pkdump_db::open_shared(&db_path)?;

    // The run's clock, read ONCE — see `pkdump_derive::clock`. It picks the
    // ingest_date partition, it is recorded in every manifest, and it is what
    // every fetched_at / observed_at column this run writes gets.
    let clock = DeriveClock::now();

    // Resolved before anything is fetched: a landing zone that was asked for
    // and is not configured should stop the run at the start, not after an
    // hour of requests whose bytes then have nowhere to go.
    let landing = crate::landing::open(args.land_raw, &clock)?;

    // 1-3b. The acquisition phase — every step that reaches an upstream we
    //        keep bytes from. Bracketed so the landing zone's manifests are
    //        written whichever way it ends; everything after it is local
    //        derivation, whose failure says nothing about whether the raw
    //        bytes arrived.
    let acquired = acquire(&mut conn, &args, &clock, landing.as_ref());
    if let Some(landing) = &landing {
        pkdump_derive::finalize_landing(landing, acquired.as_ref().err())?;
    }
    acquired?;

    // 4. Reconcile the variants lookup table — re-apply data/variants.json
    //    and synthesize rows for any set-specific stamp codes already in
    //    `printings` from prior runs, so the FK on printings.variant is
    //    satisfied before expansion writes more.
    println!("Reconciling variants table...");
    let n_variants = pkdump_db::variants::reconcile(&mut conn)?;
    println!("  {n_variants} variants known");

    // 4b. Reconcile the TCGCSV (group, sub_type) → variant lookup table —
    //     authored in data/tcgcsv_sub_type_variants.json. Must run after
    //     variants::reconcile (the FK is on variants.code) and before
    //     expand_all_printings consults it. See pokedumpster-5is.
    println!("Reconciling tcgcsv_sub_type_variant_map...");
    let n_sub = pkdump_db::sub_type_map::reconcile(&mut conn)?;
    println!("  {n_sub} (group, sub_type) → variant rows");

    // 4c. Reconcile the bundles registry from data/bundles.json. The
    //     /api/sets dispatch is driven by `bundles.slug`, so this must
    //     run before serving traffic.
    println!("Reconciling bundles table from data/bundles.json...");
    let n_bundles = pkdump_db::bundles::reconcile(&mut conn)?;
    println!("  {n_bundles} bundles registered");

    // 4d. Report the search query language metadata (keywords, rarity ranks,
    //     is:-flag definitions) seeded from data/search_*.json — it powers the
    //     collection search bar's parser/compiler and autocomplete (decision
    //     D1/D2). The read-write open at the top of this function reconciled
    //     it (connection.rs::converge), so this reads the result back rather
    //     than rewriting a few hundred rows to print a number (pd-dzu5).
    let sm = pkdump_db::search_meta::counts(&conn)?;
    println!(
        "  {} keywords, {} rarities, {} flags",
        sm.keywords, sm.rarities, sm.flags
    );

    // 4e. Auto-discover sets TCGCSV has published and pokemontcg.io
    //     hasn't — a numbered expansion group that bridges to nothing
    //     becomes a set + cards on its own (pd-558b1e4f). Local: it reads
    //     the TCGCSV products imported in step 3, so `--skip-prices`
    //     simply leaves it with nothing to find.
    println!("Discovering new sets from unbridged TCGCSV groups...");
    for d in pkdump_ingest::set_discovery::discover_new_sets(&mut conn)? {
        println!(
            "  {} ({}) — {} from group {}, {} cards",
            d.set_code, d.series, d.name, d.group_id, d.cards
        );
    }

    // 5. Synthesize card rows for bridged TCGCSV groups whose upstream
    //    pokemontcg.io entry doesn't exist yet (e.g. MEP). Idempotent
    //    INSERT OR IGNORE — when upstream catches up, the real cards
    //    win and stubs stand down on the next refresh.
    println!("Synthesizing cards for bridged groups...");
    let n_synth = tcgcsv::synthesize_cards_for_bridges(&mut conn)?;
    println!("  {n_synth} cards synthesized");

    // 5b. Curated standalone promos (Ancient Mew, etc.) — setless cards
    //     that can't bridge onto a base card. expand_all_printings skips
    //     the promo set, so this owns those printings end to end.
    let n_promo = standalone_promos::synthesize_standalone_promos(&mut conn)?;
    println!("  {n_promo} standalone promos synthesized");

    // 6. Variant expansion. TCGCSV-derived first (each printing carries
    //    its sub_type_name + tcgplayer_product_id), overlay on top for
    //    cards TCGCSV can't model (stamps, etc.).
    println!("Expanding variants into printings...");
    let overlay = overrides::load_variant_augmentations()?;
    let printings = overrides::expand_all_printings(&mut conn, &overlay, clock.fetched_at())?;
    println!("  wrote {printings} printings");

    // 6b. Report sets that mapped no printing to a TCGplayer product at
    //     all — the shape `basep` sat in, unnoticed, for the catalog's
    //     whole life (pd-0o5m). See `coverage`.
    println!("Checking TCGplayer mapping coverage...");
    coverage::report_unmapped_sets(&conn)?;

    // 7. Normalize set symbol glyphs — trim transparent padding off the
    //    upstream PNGs and self-host at a uniform target height so the
    //    /browse tiles render consistently. See `symbols.rs`.
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

    // Materialize latest_prices so the per-row market-price lookup on the
    // collection/search/binder pages is an indexed point read (vi37).
    println!("Refreshing materialized latest_prices...");
    let n_latest = pkdump_db::latest_prices::refresh_latest_prices(&conn)?;
    println!("  {n_latest} latest-price rows materialized");

    // The seeds whose rows FK into rows this run created: curated prices
    // (into `printings`, which only exist after variant expansion above —
    // pd-m4gw) and set-name aliases (into `sets`). `open_shared` reconciled
    // them against an empty catalog on the way in and so wrote nothing; run
    // here they land in the same run that created their targets, which is
    // what makes one setup a fixed point rather than the first of two
    // (pd-zg7o).
    let seeds = pkdump_db::reconcile_ingest_dependent_seeds(&mut conn)?;
    println!(
        "  {} curated catalog price overrides, {} set aliases reconciled",
        seeds.catalog_prices, seeds.set_aliases
    );

    // Same reason as the derivation's own last act (pd-t50h): `setup` writes
    // the whole catalog and then exits, and nothing that runs afterwards will
    // ever checkpoint what it left behind. Cheap here — a cold build has no
    // readers — and the point is that it does not depend on that being true.
    pkdump_db::reclaim_catalog_wal(&conn)?;

    println!("Setup complete: {}", db_path.display());
    Ok(())
}

/// Everything in `setup` that reaches an upstream whose bytes we keep: the
/// pokemon-tcg-data tarball, the pokemontcg.io tail, and both TCGCSV
/// categories.
///
/// Separated from the rest of `run` because acquiring and deriving are
/// different jobs with different failure meanings: a fetch that fails leaves
/// the raw prefix short and its manifest has to say so, while a variant
/// expansion that fails says nothing about the bytes, which are already
/// landed and complete.
///
/// `symbols::normalize_all_symbols` also fetches, from
/// `images.pokemontcg.io`, and is deliberately *not* here: card art and set
/// symbols are excluded from the landing zone, because the retention
/// arithmetic that justifies keeping `raw/` forever is for JSON only.
fn acquire(
    conn: &mut Connection,
    args: &SetupArgs,
    clock: &DeriveClock,
    landing: Option<&Arc<RawLanding>>,
) -> anyhow::Result<()> {
    let wire = crate::landing::wire(landing);
    // 1. Bulk catalog import.
    let stats = match &args.from_dir {
        Some(dir) => {
            println!("Importing pokemon-tcg-data from {}", dir.display());
            pokemon_tcg_data::import_from_dir(conn, dir, clock.fetched_at())?
        }
        None => {
            println!("Downloading the pokemon-tcg-data repo...");
            pokemon_tcg_data::download_and_import(conn, &wire, clock.fetched_at())?
        }
    };
    println!("  imported {} sets, {} cards", stats.sets, stats.cards);

    // 2. pokemontcg.io tail.
    if args.skip_tail {
        println!("Skipping pokemontcg.io tail fetch.");
    } else {
        println!("Filling newest sets from pokemontcg.io...");
        let added = import_tail(conn, clock, wire.clone())?;
        println!("  added {added} set(s) not yet in the repo");
    }

    // 3. TCGCSV groups, sealed products, single-card products, prices.
    //    Variant expansion (step 4) reads this back out as its
    //    authoritative source.
    if args.skip_prices {
        println!("Skipping TCGCSV import.");
    } else {
        println!("Importing TCGCSV groups, products, prices...");
        let r = import_tcgcsv(conn, clock, wire.clone())?;
        println!(
            "  {} groups, {} sealed products, {} card products, {} price rows",
            r.0, r.1, r.2, r.3
        );
    }

    // 3b. Pokémon Japan (TCGCSV categoryId 85). No pokemontcg.io
    //     counterpart exists, so sets and cards are synthesized straight
    //     from TCGCSV — see `pkdump_ingest::japan`. Runs after the
    //     English pass so the two never contend for a set_code.
    if args.skip_prices || args.skip_japan {
        println!("Skipping the Pokémon Japan catalog.");
    } else {
        println!("Importing the Pokémon Japan catalog (TCGCSV category 85)...");
        let j = japan::import_all(
            conn,
            clock.fetched_at(),
            clock.observed_date(),
            wire.clone(),
        )?;
        println!(
            "  {} groups, {} cards, {} card products, {} sealed products, {} price rows",
            j.groups, j.cards, j.card_products, j.sealed_products, j.price_rows
        );
    }

    Ok(())
}

/// Fetch the pokemontcg.io set list and import any set the repo did not have.
///
/// A set row with no `ptcgio_fetched_at` was synthesized locally (a bridge
/// entry, or TCGCSV set discovery running ahead of upstream) and counts as
/// missing — importing it is how the real cards supersede the stubs.
fn import_tail(
    conn: &mut Connection,
    clock: &DeriveClock,
    wire: pkdump_ingest::landing::Wire,
) -> anyhow::Result<usize> {
    let client = PokemonTcgClient::new()?.on_wire(wire);
    let now = clock.fetched_at();
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
    clock: &DeriveClock,
    wire: pkdump_ingest::landing::Wire,
) -> anyhow::Result<(usize, usize, usize, usize)> {
    let client = TcgcsvClient::new()?.on_wire(wire);
    let now = clock.fetched_at();
    let observed = clock.observed_date();

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

/// `pkdump setup`'s acquisition path, exercised against a fake upstream.
///
/// The nightly `raw_coverage` gate (in `data.rs`) audits `pkdump data refresh`
/// — the landing-only path. `pkdump setup` uses different code: its own
/// `import_tail` (which imports as well as fetches), `download_and_import`
/// rather than `land_bulk`, and a full per-group TCGCSV import. This gate does
/// for setup what `raw_coverage` does for refresh.
///
/// The upstream has two sets:
/// - sv3pt5 in the bulk corpus and in the pokemontcg.io list.
/// - svp ONLY in the pokemontcg.io list — the lag-window set not yet in the
///   bulk corpus.
///
/// After `download_and_import` sv3pt5 is in the catalog with
/// `ptcgio_fetched_at IS NOT NULL`, so `import_tail` skips it and fetches
/// only svp's cards. The gate asserts that sv3pt5's cards are NOT fetched a
/// second time and svp's ARE.
///
/// An exhaustive match on [`Dataset`] is the compile-time half: a new variant
/// requires somebody to classify it before the test compiles, so a new
/// upstream input cannot be invisible to this gate.
#[cfg(test)]
mod setup_coverage {
    use super::*;
    use std::sync::{Arc, MutexGuard};

    use pkdump_ingest::test_upstream::{FakeUpstream, Reply};
    use pkdump_ingest::upstream::{
        ENV_POKEMON_TCG_DATA_BASE_URL, ENV_POKEMONTCG_BASE_URL, ENV_TCGCSV_BASE_URL,
    };
    use pkdump_lake::{Dataset, DirStore, Manifest, RawLanding};

    /// Classification of each dataset's role in a `pkdump setup` run.
    #[allow(dead_code)]
    enum SetupNeeds {
        /// Setup always fetches this from the network on any run.
        Always,
        /// Fetched only when a new set has been published since the last bulk
        /// corpus snapshot. On a fully-current bulk corpus, `import_tail` still
        /// fetches the pokemontcg.io sets list but finds nothing new to import,
        /// so [`Dataset::Cards`] would not appear in the manifests. The test
        /// exercises this path by serving svp, a set absent from the bulk
        /// tarball, so the cards manifest IS expected here.
        LagWindow(&'static str),
    }

    /// Exhaustive by design: adding a [`Dataset`] variant requires classifying
    /// it here before the test compiles, so a new upstream input cannot be
    /// invisible to this gate (the same guarantee `raw_coverage` gives for the
    /// nightly refresh).
    fn classification(dataset: Dataset) -> SetupNeeds {
        match dataset {
            Dataset::Bulk
            | Dataset::Sets
            | Dataset::Groups
            | Dataset::Products
            | Dataset::Prices => SetupNeeds::Always,
            Dataset::Cards => SetupNeeds::LagWindow(
                "setup fetches cards from pokemontcg.io only for sets the bulk corpus \
                 hasn't caught up to yet — the same lag window pkdump data refresh's \
                 tail covers. The test exercises this path by serving svp, a set that \
                 is in pokemontcg.io but absent from the bulk tarball.",
            ),
        }
    }

    const INGEST_DATE: &str = "2026-09-21";
    const CLOCK_AT: &str = "2026-09-21T06:00:00Z";

    // sv3pt5 is in the bulk corpus and pokemontcg.io.
    // svp is the lag-window set: in pokemontcg.io only, absent from the bulk tarball.
    const SETS: &str = r#"{"data":[
        {"id":"sv3pt5","name":"151","series":"Scarlet & Violet",
         "printedTotal":165,"total":207,"ptcgoCode":"MEW",
         "releaseDate":"2023/09/22"},
        {"id":"svp","name":"SVP Black Star Promos","series":"Scarlet & Violet",
         "printedTotal":999,"total":999,"ptcgoCode":"SVP",
         "releaseDate":"2023/11/03"}],
        "page":1,"pageSize":250,"count":2,"totalCount":2}"#;

    // Cards for the lag-window set.
    const SVP_CARDS: &str = r#"{"data":[
        {"id":"svp-1","name":"Pikachu","supertype":"Pokémon","subtypes":["Basic"],
         "hp":"70","types":["Lightning"],"number":"1","rarity":"Promo",
         "set":{"id":"svp","name":"SVP Black Star Promos","series":"Scarlet & Violet"},
         "tcgplayer":{"prices":{"holofoil":{}}}}],
        "page":1,"pageSize":250,"count":1,"totalCount":1}"#;

    // Bulk tarball: sv3pt5 only — svp is the lag-window set not yet in the corpus.
    const BULK_SETS: &str = r#"[{"id":"sv3pt5","name":"151",
        "series":"Scarlet & Violet","printedTotal":165,"total":207,
        "ptcgoCode":"MEW","releaseDate":"2023/09/22"}]"#;
    const BULK_CARDS: &str = r#"[{"id":"sv3pt5-4","name":"Charmander",
        "supertype":"Pokémon","subtypes":["Basic"],"hp":"60",
        "types":["Fire"],"number":"4","rarity":"Common"}]"#;

    const ENGLISH_GROUPS: &str = r#"{"results":[
        {"groupId":23237,"name":"SV: 151","abbreviation":"MEW",
         "publishedOn":"2023-09-22"}],"success":true,"errors":[]}"#;
    const JAPAN_GROUPS: &str = r#"{"results":[
        {"groupId":23099,"name":"SV2a: Pokemon Card 151","abbreviation":"",
         "publishedOn":"2023-06-16"}],"success":true,"errors":[]}"#;
    const EMPTY: &str = r#"{"results":[],"success":true,"errors":[]}"#;

    fn build_bulk_tarball() -> Vec<u8> {
        use flate2::{Compression, write::GzEncoder};

        let gz_buf = Vec::new();
        let enc = GzEncoder::new(gz_buf, Compression::default());
        let mut builder = tar::Builder::new(enc);

        let add = |b: &mut tar::Builder<GzEncoder<Vec<u8>>>, path: &str, data: &[u8]| {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            b.append_data(&mut header, path, std::io::Cursor::new(data))
                .expect("tar append");
        };
        add(
            &mut builder,
            "pokemon-tcg-data-master/sets/en.json",
            BULK_SETS.as_bytes(),
        );
        add(
            &mut builder,
            "pokemon-tcg-data-master/cards/en/sv3pt5.json",
            BULK_CARDS.as_bytes(),
        );
        let enc = builder.into_inner().expect("tar finish");
        enc.finish().expect("gz finish")
    }

    /// Both upstreams on one server.  TCGCSV lives under `/<category>/…`,
    /// pokemontcg.io under `/v2`, and the bulk tarball under the GitHub path.
    /// A 404 for an unexpected path is the gate's "new call-site with no
    /// landing" detector.
    fn route(target: &str, _n: usize) -> Reply {
        let path = target.split('?').next().unwrap_or(target);
        match path {
            "/3/groups" => Reply::ok(ENGLISH_GROUPS),
            "/85/groups" => Reply::ok(JAPAN_GROUPS),
            p if p.ends_with("/products") || p.ends_with("/prices") => Reply::ok(EMPTY),
            "/v2/sets" => Reply::ok(SETS),
            "/v2/cards" => Reply::ok(SVP_CARDS),
            p if p.contains("pokemon-tcg-data") => Reply {
                status: 200,
                body: build_bulk_tarball(),
                content_type: "application/x-tar",
            },
            other => Reply::status(
                404,
                format!(
                    r#"{{"error":"setup asked for {other}, which this fixture does not \
                        model — add a route here and a classification in setup_coverage"}}"#
                ),
            ),
        }
    }

    struct Origins<'a>(#[allow(dead_code)] MutexGuard<'a, ()>);

    impl Origins<'_> {
        fn point_at(base: &str) -> Self {
            let guard = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: the lock is held for as long as the variables are set.
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
            // SAFETY: still under the lock this value holds.
            unsafe {
                std::env::remove_var(ENV_TCGCSV_BASE_URL);
                std::env::remove_var(ENV_POKEMONTCG_BASE_URL);
                std::env::remove_var(ENV_POKEMON_TCG_DATA_BASE_URL);
            }
        }
    }

    fn manifest_for(manifests: &[Manifest], dataset: Dataset) -> Option<&Manifest> {
        manifests.iter().find(|m| m.dataset == dataset.as_str())
    }

    fn strip_base(url: &str, base: &str) -> String {
        url.strip_prefix(base).unwrap_or(url).to_string()
    }

    /// `pkdump setup`'s acquisition fetches every dataset a fresh catalog
    /// needs, lands every byte it receives, and exercises the lag-window
    /// path (cards for sets the bulk corpus hasn't caught up to yet).
    #[test]
    fn setup_acquires_every_upstream_input_a_fresh_catalog_needs() {
        let upstream = FakeUpstream::start(route);
        let _origins = Origins::point_at(&upstream.base_url());
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("shared.sqlite");
        let land_dir = tmp.path().join("raw");

        let clock = pkdump_derive::DeriveClock::from_manifest(CLOCK_AT, "setup_coverage clock")
            .expect("parse clock");
        let landing = Arc::new(RawLanding::new(
            Box::new(DirStore::new(&land_dir)),
            INGEST_DATE,
            clock.fetched_at(),
        ));

        let mut conn = pkdump_db::open_shared(&db_path).unwrap();

        let args = SetupArgs {
            from_dir: None,
            skip_tail: false,
            skip_prices: false,
            skip_japan: false,
            db: None,
            land_raw: false, // landing is passed directly — land_raw is not needed
        };

        let outcome = acquire(&mut conn, &args, &clock, Some(&landing));
        pkdump_derive::finalize_landing(&landing, outcome.as_ref().err()).expect("write manifests");
        outcome.expect("setup acquisition succeeded");

        let manifests = landing.manifests();

        // 1. Exhaustive classification: every dataset setup acquires is present
        //    with at least one landed part. The match is exhaustive so a new
        //    Dataset variant cannot slip past without being classified here.
        for dataset in Dataset::ALL {
            match classification(*dataset) {
                SetupNeeds::Always | SetupNeeds::LagWindow(_) => {
                    let m = manifest_for(&manifests, *dataset).unwrap_or_else(|| {
                        panic!(
                            "setup landed nothing for {dataset} — add it to acquire() and \
                             classify it in setup_coverage::classification()"
                        )
                    });
                    assert!(!m.parts.is_empty(), "{dataset}: manifest has no parts");
                    assert!(m.complete, "{dataset}: manifest is incomplete");
                }
            }
        }

        // 2. Everything the upstream served was stored — the call-site audit.
        //    A fetch added to acquire() that bypasses landing::fetch_bytes shows
        //    up as a served URL absent from every manifest.
        let mut served: Vec<String> = upstream
            .requests()
            .into_iter()
            .map(|u| strip_base(&u, &upstream.base_url()))
            .collect();
        served.sort();
        let mut landed: Vec<String> = manifests
            .iter()
            .flat_map(|m| {
                m.parts
                    .iter()
                    .map(|p| strip_base(&p.url, &upstream.base_url()))
            })
            .collect();
        landed.sort();
        assert_eq!(
            served, landed,
            "every upstream response setup receives must be landed exactly once"
        );

        // 3. The lag-window path was exercised: cards for svp were fetched but
        //    NOT for sv3pt5, which was already in the catalog from the bulk tarball.
        //    The pokemontcg.io client percent-encodes the colon in `set.id:svp`
        //    to `%3A` in the query string, so we check for the encoded form.
        let cards_m =
            manifest_for(&manifests, Dataset::Cards).expect("cards manifest for lag-window set");
        assert!(
            cards_m.parts.iter().any(|p| p.url.contains("set.id%3Asvp")),
            "setup must fetch cards for the lag-window set (svp): {:?}",
            cards_m.parts.iter().map(|p| &p.url).collect::<Vec<_>>()
        );
        assert!(
            !cards_m.parts.iter().any(|p| p.url.contains("sv3pt5")),
            "setup must NOT re-fetch cards for sv3pt5 (already in catalog from bulk tarball): \
             {:?}",
            cards_m.parts.iter().map(|p| &p.url).collect::<Vec<_>>()
        );

        // 4. Japanese TCGCSV (category 85) shares `source=tcgcsv` with
        //    English (category 3) — same check as raw_coverage.
        for dataset in [Dataset::Groups, Dataset::Products, Dataset::Prices] {
            let m = manifest_for(&manifests, dataset).expect("tcgcsv manifest");
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

        // 5. Both sets acquired by setup are in the catalog: sv3pt5 from the
        //    bulk tarball and svp from import_tail. The bridge overlay in
        //    import_groups may also synthesize other sets (like "mep"), so
        //    the Japan import adds jp- prefixed rows; we only assert on what
        //    acquire() directly imported.
        let has_sv3pt5: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sets WHERE set_code = 'sv3pt5'",
                [],
                |r| r.get::<_, i64>(0).map(|n| n > 0),
            )
            .unwrap();
        let has_svp: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sets WHERE set_code = 'svp'",
                [],
                |r| r.get::<_, i64>(0).map(|n| n > 0),
            )
            .unwrap();
        assert!(has_sv3pt5, "sv3pt5 (bulk tarball) must be in the catalog");
        assert!(has_svp, "svp (pokemontcg.io tail) must be in the catalog");
        let svp_cards: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM cards WHERE set_code = 'svp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            svp_cards, 1,
            "svp's card from pokemontcg.io must be in the catalog"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETS_FIXTURE: &str = r#"[
      {"id":"sv3pt5","name":"151","series":"Scarlet & Violet",
       "printedTotal":165,"total":207,"ptcgoCode":"MEW",
       "releaseDate":"2023/09/22"}
    ]"#;

    const CARDS_FIXTURE: &str = r#"[
      {"id":"sv3pt5-1","name":"Bulbasaur","supertype":"Pokémon",
       "subtypes":["Basic"],"hp":"70","types":["Grass"],"number":"1",
       "rarity":"Common",
       "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"},
       "tcgplayer":{"prices":{"normal":{},"reverseHolofoil":{}}}},
      {"id":"sv3pt5-4","name":"Charizard ex","supertype":"Pokémon",
       "subtypes":["Basic","ex"],"hp":"330","types":["Fire"],"number":"4",
       "rarity":"Double Rare",
       "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"},
       "tcgplayer":{"prices":{"holofoil":{}}}}
    ]"#;

    #[test]
    fn setup_from_dir_offline_builds_catalog() {
        // A minimal pokemon-tcg-data checkout.
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join("sets")).unwrap();
        std::fs::create_dir_all(repo.path().join("cards").join("en")).unwrap();
        std::fs::write(repo.path().join("sets").join("en.json"), SETS_FIXTURE).unwrap();
        std::fs::write(
            repo.path().join("cards").join("en").join("sv3pt5.json"),
            CARDS_FIXTURE,
        )
        .unwrap();

        let dbdir = tempfile::tempdir().unwrap();
        let db_path = dbdir.path().join("shared.sqlite");

        run(SetupArgs {
            from_dir: Some(repo.path().to_path_buf()),
            skip_tail: true,
            skip_prices: true,
            skip_japan: true,
            db: Some(db_path.clone()),
            land_raw: false,
        })
        .unwrap();

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let sets: i64 = conn
            .query_row("SELECT count(*) FROM sets", [], |r| r.get(0))
            .unwrap();
        let cards: i64 = conn
            .query_row("SELECT count(*) FROM cards", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sets, 1);
        assert_eq!(cards, 2);

        // Variant expansion ran. skip_prices means TCGCSV is absent, so
        // each card falls back to the bare `normal` placeholder; the 151
        // overlay then adds pokeball_rh + masterball_rh for Bulbasaur
        // (Common) and stops at `normal` for Charizard ex (no overlay
        // rule). A real refresh with TCGCSV present would replace these
        // with the true sub_type-derived variants.
        let bulbasaur: i64 = conn
            .query_row(
                "SELECT count(*) FROM printings \
                 WHERE card_id = 'sv3pt5-1' AND deprecated_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bulbasaur, 3);

        let charizard: i64 = conn
            .query_row(
                "SELECT count(*) FROM printings \
                 WHERE card_id = 'sv3pt5-4' AND deprecated_at IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(charizard, 1);
    }
}
