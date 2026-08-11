//! A whole data directory — catalog, registry, tenants — plus the value
//! snapshot **Rust** computes from it.
//!
//! pd-ruwh replaces `pkdump data refresh` step 7 (one tenant, no loop) with a
//! lake transform that snapshots *every* registered tenant. Two claims have to
//! be checked, and neither can be checked from inside the transform:
//!
//! 1. the rewrite is **observably a no-op** — for the same date and the same
//!    tenant it must reproduce `value_history::snapshot_today` exactly, and
//! 2. **every** tenant gets rows, which is the bug (pd-s5yn) inverted.
//!
//! So this builds the input to both and the answer to the first: a raw landing
//! zone, the `shared.sqlite` imported from the *same bytes*, a registry with
//! three users, two collections, and a dump of the snapshot rows Rust produces
//! for each date. `tests/lake/value_snapshots.sh` then runs the Python job over
//! the same directory and diffs.
//!
//! It is a test in its own right — run bare it asserts what it built into a
//! temp dir. Point `PKDUMP_VALUE_FIXTURE_OUT` at a directory and it writes
//! there instead, which is how the container gate gets its input:
//!
//! ```sh
//! PKDUMP_VALUE_FIXTURE_OUT=/tmp/fixture \
//!   cargo test -p pkdump-ingest --test value_snapshot_fixture
//! ```
//!
//! ## What each piece is shaped for
//!
//! | piece | why it is there |
//! | --- | --- |
//! | two priced dates | the older one is the backfill proof: it must reconstruct *that* day, not today |
//! | a printing with no TCGplayer product | the `manual_prices` arm of the market-price COALESCE |
//! | copies in two sets, one binder | the `set` and `binder` dimensions have more than one bucket |
//! | a `sold` copy and a `Lightly Played` one | status filtering and the condition multiplier |
//! | **three** registered tenants | two with collections, and one whose file is gone — a run must skip it and say so |
//!
//! The tenants are provisioned by the real `pkdump_db::tenants::create`, so
//! the registry rows, the opaque ids and the `tenants/<id>.sqlite` layout are
//! production's and not this file's idea of them. The Python job resolves that
//! layout independently; a change to it breaks this gate, which is the point.

// Each integration test compiles `support` separately, and this one needs
// neither the request log nor the failure injection the others assert on.
#[allow(dead_code)]
mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::{Connection, params};

use pkdump_ingest::tcgcsv::{self, TcgcsvClient};
use pkdump_lake::{DirStore, RawLanding};
use support::{FakeUpstream, Reply};

/// The older priced day. A snapshot taken *for* this date must value the
/// collection at these prices — the backfill claim, made checkable.
pub const DATE_OLD: &str = "2026-08-09";
/// The newer priced day, and what `latest_prices` therefore holds.
pub const DATE_NEW: &str = "2026-08-10";

/// One group of single cards. Sealed product is deliberately absent: a
/// collection values single cards, and `catalog.prices` carrying sealed rows
/// as well is `tests/lake/prices.sh`'s business, not this gate's.
const GROUPS: &str = r#"{"success":true,"errors":[],"results":[
    {"groupId":1,"name":"Base Set","abbreviation":"BS","publishedOn":"1999-01-09T00:00:00"}
]}"#;

const PRODUCTS: &str = r#"{"success":true,"errors":[],"results":[
    {"productId":101,"groupId":1,"name":"Charizard - 4/102","imageCount":1,
     "extendedData":[{"name":"Number","value":"4/102"}]},
    {"productId":102,"groupId":1,"name":"Blastoise - 2/102","imageCount":1,
     "extendedData":[{"name":"Number","value":"2/102"}]}
]}"#;

/// Prices for one "day". Every quoted value shifts with `day`, so a snapshot
/// built from the wrong day is wrong in its *numbers* and not merely in its
/// bookkeeping — which is what makes the backfill assertion mean something.
///
/// Product 101 is quoted under two sub-types and 102 under none (`null` →
/// `Normal`, the default both importers apply). The nulls are prices that do
/// not exist and must produce no row on either side.
fn prices_json(day: usize) -> String {
    let d = day as f64;
    format!(
        r#"{{"success":true,"errors":[],"results":[
            {{"productId":101,"subTypeName":"Normal","lowPrice":{:.2},"midPrice":null,
              "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}},
            {{"productId":101,"subTypeName":"Holofoil","lowPrice":null,"midPrice":null,
              "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}},
            {{"productId":102,"subTypeName":null,"lowPrice":null,"midPrice":null,
              "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}}
        ]}}"#,
        10.0 + d,
        25.50 + d,
        300.0 + d * 10.0,
        5.25 + d,
    )
}

fn start_upstream(day: Arc<AtomicUsize>) -> FakeUpstream {
    FakeUpstream::start(move |target, _n| {
        let parts: Vec<&str> = target.trim_matches('/').split('/').collect();
        match parts.as_slice() {
            ["3", "groups"] => Reply::ok(GROUPS),
            ["3", _group, "products"] => Reply::ok(PRODUCTS),
            ["3", _group, "prices"] => Reply::ok(prices_json(day.load(Ordering::SeqCst))),
            _ => Reply {
                status: 404,
                body: format!(r#"{{"error":"no route for {target}"}}"#),
            },
        }
    })
}

/// One landing run: fetch, land the bytes, and import the *same* responses
/// into `shared.sqlite`. Both sides of every downstream comparison come from
/// one pass over one set of responses — fetching twice would leave "the lake
/// and SQLite disagree" and "the upstream answered differently"
/// indistinguishable.
fn land_run(base_url: &str, raw_root: &Path, db: &Path, ingest_date: &str) {
    let landing = Arc::new(RawLanding::new(
        Box::new(DirStore::new(raw_root)),
        ingest_date,
    ));
    let client = TcgcsvClient::new()
        .expect("client")
        .base_url(base_url)
        .landing_in(Arc::clone(&landing));

    let mut conn = pkdump_db::open_shared(db).expect("open shared catalog");
    let now = format!("{ingest_date}T00:00:00Z");

    let groups = client.fetch_groups().expect("groups");
    tcgcsv::import_groups(&mut conn, &groups, &now).expect("import groups");
    for group in &groups {
        let products = client.fetch_products(group.group_id).expect("products");
        let prices = client.fetch_prices(group.group_id).expect("prices");
        tcgcsv::import_products(&mut conn, &products, &now).expect("import products");
        tcgcsv::import_prices(&mut conn, &prices, ingest_date).expect("import prices");
    }
    landing.finalize(None).expect("finalize the run's manifest");
}

/// The catalog rows a collection needs and a price import does not create:
/// two sets, four cards, and the printings that bridge them to the TCGplayer
/// products the prices are quoted for — plus one printing bridged to *nothing*,
/// which is the manual-price arm of the market-price expression.
fn seed_catalog(conn: &Connection) {
    conn.execute_batch(
        "INSERT OR REPLACE INTO sets (set_code, name, series) VALUES
             ('base1', 'Base Set', 'Base'),
             ('base2', 'Jungle',   'Base');

         INSERT OR REPLACE INTO variants (code, label, short, rank, color) VALUES
             ('normal', 'Normal',    'NRM', 1, '#888888'),
             ('holo',   'Holofoil',  'HOLO', 2, '#e94560');

         INSERT OR REPLACE INTO cards (card_id, set_code, number, number_sortable, name) VALUES
             ('base1-4',  'base1', '4',  4,  'Charizard'),
             ('base1-2',  'base1', '2',  2,  'Blastoise'),
             ('base2-64', 'base2', '64', 64, 'Pikachu'),
             ('base2-99', 'base2', '99', 99, 'Promo Meowth');

         INSERT OR REPLACE INTO printings
             (printing_id, card_id, variant, language, tcgplayer_product_id, sub_type_name) VALUES
             ('base1-4-normal',  'base1-4',  'normal', 'en', 101, 'Normal'),
             ('base1-4-holo',    'base1-4',  'holo',   'en', 101, 'Holofoil'),
             ('base1-2-normal',  'base1-2',  'normal', 'en', 102, 'Normal'),
             ('base2-64-normal', 'base2-64', 'normal', 'en', 102, 'Normal'),
             -- Priced by hand only: no product, so the market-price COALESCE
             -- has to fall through to manual_prices on both sides.
             ('base2-99-normal', 'base2-99', 'normal', 'en', NULL, NULL);

         INSERT OR REPLACE INTO conditions (name, multiplier, rank) VALUES
             ('Near Mint',         1.00, 1),
             ('Lightly Played',    0.85, 2),
             ('Moderately Played', 0.65, 3),
             ('Heavily Played',    0.45, 4),
             ('Damaged',           0.25, 5);",
    )
    .expect("seed the catalog rows a collection needs");
}

/// A collection: one binder and the copies in it.
///
/// `alice` is the prod-shaped tenant — copies across two sets, two conditions,
/// a binder, a hand-priced printing, a copy with no purchase price, and a
/// `sold` copy that must not be counted. `bob` is small and *different*, so
/// "both tenants got snapshots" cannot pass by accident on identical numbers.
fn fill_collection(conn: &Connection, rows: &[(&str, &str, Option<f64>, &str, bool)]) {
    conn.execute(
        "INSERT INTO binders (id, name, pocket_size, created_at, updated_at) \
         VALUES (1, 'Vintage', 9, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .expect("binder");
    for (printing_id, condition, purchase_price, status, in_binder) in rows {
        conn.execute(
            "INSERT INTO collection \
                 (printing_id, condition, purchase_price, acquired_at, source, status, binder_id) \
             VALUES (?1, ?2, ?3, '2026-01-02T00:00:00Z', 'manual_id', ?4, ?5)",
            params![
                printing_id,
                condition,
                purchase_price,
                status,
                if *in_binder { Some(1i64) } else { None }
            ],
        )
        .expect("collection row");
    }
    conn.execute(
        "INSERT INTO manual_prices (printing_id, price, observed_at) \
         VALUES ('base2-99-normal', 12.0, '2026-08-01')",
        [],
    )
    .expect("manual price");
}

/// The snapshot rows for one date, in the stable text form the container gate
/// diffs. Ten decimal places rather than a bare `{}`: the two implementations
/// sum the same values, and printing them at a fixed width states how closely
/// they are being held to it rather than leaving it to a float's shortest
/// representation.
fn dump_snapshot(conn: &Connection, date: &str) -> String {
    let mut stmt = conn
        .prepare(
            "SELECT dimension, COALESCE(bucket, '-'), market_value, cost_basis, card_count \
               FROM collection_value_snapshot WHERE date = ?1 \
              ORDER BY dimension, bucket",
        )
        .expect("prepare dump");
    let rows: Vec<String> = stmt
        .query_map(params![date], |r| {
            Ok(format!(
                "{}\t{}\t{:.10}\t{:.10}\t{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, f64>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })
        .expect("dump")
        .collect::<rusqlite::Result<_>>()
        .expect("dump rows");
    let mut out = rows.join("\n");
    out.push('\n');
    out
}

/// Point `latest_prices` at one day's prices, so `snapshot_today` — which
/// always reads "the latest price we know" — can be made to answer for an
/// *older* day. That is the only way to get a Rust-computed expectation for
/// the backfill date, and it is a fixture-only manoeuvre: nothing in the app
/// rewrites `latest_prices` to a past day.
fn pin_latest_prices_to(shared: &Path, date: &str) {
    let conn = pkdump_db::open_shared(shared).expect("open shared");
    conn.execute("DELETE FROM latest_prices", [])
        .expect("clear latest_prices");
    conn.execute(
        "INSERT INTO latest_prices \
             (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
         SELECT tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at \
           FROM prices WHERE observed_at = ?1",
        params![date],
    )
    .expect("pin latest_prices");
}

/// Where to build the fixture: `PKDUMP_VALUE_FIXTURE_OUT` if the caller wants
/// to keep it, otherwise a temp dir that goes away with the test.
fn out_dir() -> (PathBuf, Option<tempfile::TempDir>) {
    match std::env::var("PKDUMP_VALUE_FIXTURE_OUT") {
        Ok(path) if !path.trim().is_empty() => {
            let path = PathBuf::from(path);
            std::fs::create_dir_all(&path).expect("create the fixture directory");
            (path, None)
        }
        _ => {
            let tmp = tempfile::tempdir().expect("tempdir");
            (tmp.path().to_path_buf(), Some(tmp))
        }
    }
}

#[test]
fn builds_a_data_directory_and_the_snapshot_rust_computes_from_it() {
    let (out, _keep) = out_dir();
    let home = out.join("home");
    let raw_root = out.join("raw-zone");
    let shared = home.join("shared.sqlite");
    // A fixture directory is reused across runs; stale databases would make
    // every count below depend on how many times this has run.
    let _ = std::fs::remove_dir_all(&raw_root);
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("create the data directory");

    // The tenant layout is production's: `pkdump_db::paths` reads $PKDUMP_HOME,
    // and `tenants::create` is what prod runs.
    // SAFETY: this test binary is one process and sets it before any thread
    // that reads it exists.
    unsafe { std::env::set_var("PKDUMP_HOME", &home) };

    // -- the catalog, from landed bytes ---------------------------------
    let day = Arc::new(AtomicUsize::new(1));
    let upstream = start_upstream(Arc::clone(&day));
    let base = upstream.base_url();
    land_run(&base, &raw_root, &shared, DATE_OLD);
    day.store(2, Ordering::SeqCst);
    land_run(&base, &raw_root, &shared, DATE_NEW);

    {
        let conn = pkdump_db::open_shared(&shared).expect("open shared");
        seed_catalog(&conn);
        let n = pkdump_db::latest_prices::refresh_latest_prices(&conn).expect("latest_prices");
        assert!(
            n > 0,
            "latest_prices must be materialized for the Rust side"
        );
        let market: f64 = conn
            .query_row(
                "SELECT price FROM latest_prices WHERE tcgplayer_product_id = 101 \
                   AND sub_type_name = 'Normal' AND price_type = 'market'",
                [],
                |r| r.get(0),
            )
            .expect("latest market price");
        assert_eq!(
            market, 27.5,
            "latest_prices must hold the NEWER day — the whole backfill assertion \
             rests on the two days differing"
        );
    }

    // -- three tenants, two collections ---------------------------------
    let alice = pkdump_db::tenants::create("alice").expect("create alice");
    let bob = pkdump_db::tenants::create("bob").expect("create bob");
    let ghost = pkdump_db::tenants::create("ghost").expect("create ghost");

    {
        let conn = pkdump_db::connect_user(&alice.path, &shared).expect("open alice");
        fill_collection(
            &conn,
            &[
                ("base1-4-holo", "Near Mint", Some(150.0), "owned", true),
                ("base1-4-normal", "Lightly Played", Some(9.0), "owned", true),
                ("base1-2-normal", "Near Mint", None, "owned", false),
                ("base2-64-normal", "Near Mint", Some(3.0), "owned", false),
                ("base2-99-normal", "Near Mint", Some(4.0), "owned", false),
                // Sold, and therefore not part of what the collection is worth.
                ("base1-4-holo", "Near Mint", Some(500.0), "sold", false),
            ],
        );
    }
    {
        let conn = pkdump_db::connect_user(&bob.path, &shared).expect("open bob");
        fill_collection(
            &conn,
            &[(
                "base1-2-normal",
                "Moderately Played",
                Some(1.0),
                "owned",
                true,
            )],
        );
    }

    // A registered user whose database is gone: the run must skip them, keep
    // going, and say so in its exit status.
    for sidecar in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{sidecar}", ghost.path.display()));
    }
    assert!(!ghost.path.exists(), "ghost's database must be missing");

    // -- what Rust computes, for both dates -----------------------------
    // DATE_NEW first, from `latest_prices` as it really stands: this is the
    // "observably a no-op" expectation, and alice's rows are LEFT IN PLACE so
    // the transform has to reproduce them rather than merely produce something.
    let expected_new = {
        let mut conn = pkdump_db::connect_user(&alice.path, &shared).expect("open alice");
        let n = pkdump_db::value_history::snapshot_today(&mut conn, DATE_NEW).expect("snapshot");
        // Counted by hand from the collection above: one 'all', two sets
        // (base1 holds three owned copies, base2 two), one binder.
        assert_eq!(n, 4, "1 'all' + 2 sets + 1 binder");
        dump_snapshot(&conn, DATE_NEW)
    };

    // DATE_OLD next, with `latest_prices` pinned to the older day — the same
    // Rust aggregate over the prices that day actually quoted. Its rows are
    // then REMOVED: the transform has to reconstruct history that was never
    // captured, which is the backfill claim.
    pin_latest_prices_to(&shared, DATE_OLD);
    let expected_old = {
        let mut conn = pkdump_db::connect_user(&alice.path, &shared).expect("open alice");
        pkdump_db::value_history::snapshot_today(&mut conn, DATE_OLD).expect("snapshot");
        let dump = dump_snapshot(&conn, DATE_OLD);
        conn.execute(
            "DELETE FROM collection_value_snapshot WHERE date = ?1",
            params![DATE_OLD],
        )
        .expect("remove the older day");
        dump
    };
    // Put the catalog back the way a real one stands, so the fixture the gate
    // consumes is not carrying a fixture-only manoeuvre in its state.
    {
        let conn = pkdump_db::open_shared(&shared).expect("open shared");
        pkdump_db::latest_prices::refresh_latest_prices(&conn).expect("restore latest_prices");
    }

    assert_ne!(
        expected_new, expected_old,
        "the two days must value the collection differently, or the backfill \
         assertion downstream proves nothing"
    );

    std::fs::write(
        out.join(format!("expected-alice-{DATE_NEW}.tsv")),
        &expected_new,
    )
    .expect("write the DATE_NEW expectation");
    std::fs::write(
        out.join(format!("expected-alice-{DATE_OLD}.tsv")),
        &expected_old,
    )
    .expect("write the DATE_OLD expectation");
    std::fs::write(
        out.join("tenants.tsv"),
        format!(
            "alice\t{}\nbob\t{}\nghost\t{}\n",
            alice.user.database_id, bob.user.database_id, ghost.user.database_id
        ),
    )
    .expect("write the tenant map");

    // -- what the fixture holds, asserted --------------------------------
    let conn = Connection::open(&alice.path).expect("open alice");
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM collection_value_snapshot WHERE date = ?1",
            params![DATE_NEW],
            |r| r.get::<_, i64>(0)
        )
        .expect("count"),
        4,
        "alice keeps the DATE_NEW rows for the byte-identity comparison"
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM collection_value_snapshot", [], |r| {
            r.get::<_, i64>(0)
        })
        .expect("count"),
        4,
        "and only those — the older day is the transform's to reconstruct"
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM collection_value_snapshot_run",
            [],
            |r| r.get::<_, i64>(0)
        )
        .expect("count"),
        0,
        "no provenance row yet: it is how the gate knows the transform ran at all"
    );

    let bob_conn = Connection::open(&bob.path).expect("open bob");
    assert_eq!(
        bob_conn
            .query_row("SELECT COUNT(*) FROM collection_value_snapshot", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("count"),
        0,
        "bob has no value history at all — that IS the bug (pd-s5yn)"
    );

    assert!(
        raw_root
            .join(format!(
                "raw/source=tcgcsv/dataset=prices/ingest_date={DATE_NEW}"
            ))
            .is_dir(),
        "the landing zone must hold the newer day for the lake to build from"
    );

    if let Ok(path) = std::env::var("PKDUMP_VALUE_FIXTURE_OUT") {
        println!("fixture written to {path}");
        println!("{expected_new}");
    }
}
