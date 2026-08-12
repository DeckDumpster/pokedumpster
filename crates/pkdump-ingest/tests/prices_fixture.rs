//! A raw landing zone and the `shared.sqlite` built from the *same bytes*.
//!
//! pd-1ojt's build job (`lake/src/pkdump_lake/prices.py`) claims it can
//! rebuild `catalog.prices` from `raw/` alone, and `tests/lake/prices.sh`
//! checks that claim against SQLite. Both need a landing zone to read — and
//! where that landing zone comes from decides what the gate can actually
//! catch.
//!
//! Hand-writing zstd parts and a manifest in the shell would be easier and
//! would prove nothing: it would assert that the Python reader can read
//! **the shell's** idea of the layout. So the fixture is produced here, by
//! the real `TcgcsvClient` through the real `RawLanding`, and the SQLite side
//! by the real `import_prices`. Change the key layout, the manifest fields,
//! the zstd level or the price parser, and the gate downstream notices.
//!
//! It is a test in its own right — run bare, it asserts what it built into a
//! temp dir. Point `PKDUMP_PRICES_FIXTURE_OUT` at a directory and it writes
//! there instead, which is how the container gate gets its input:
//!
//! ```sh
//! PKDUMP_PRICES_FIXTURE_OUT=/tmp/fixture \
//!   cargo test -p pkdump-ingest --test prices_fixture
//! ```
//!
//! Three dates, each shaped to make a different assertion possible:
//!
//! | date | what landed | what it is for |
//! | --- | --- | --- |
//! | [`DATE_INCOMPLETE`] | one run that died on its second group | a build must refuse rather than pass off a partial day as whole |
//! | [`DATE_OLD`] | one complete run | rebuilding an older date reproduces *that* day |
//! | [`DATE_NEW`] | a complete run, then a later retry that died | the complete run wins over a newer incomplete one |
//!
//! Prices differ per date, and the incomplete retry on [`DATE_NEW`] carries
//! *different* prices again — so a builder that quietly stitched runs
//! together would produce visibly wrong numbers rather than merely wrong
//! bookkeeping.

// Each integration test compiles `support` separately, and this one does not
// need the request log that `raw_landing` asserts on.
#[allow(dead_code)]
mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use pkdump_ingest::tcgcsv::{self, TcgcsvClient};
use pkdump_lake::{DirStore, RawLanding};
use support::{FakeUpstream, Reply};

/// Only an incomplete run landed here.
pub const DATE_INCOMPLETE: &str = "2026-08-08";
/// A complete run, and the date a rebuild must be able to reproduce.
pub const DATE_OLD: &str = "2026-08-09";
/// A complete run followed by a failed retry.
pub const DATE_NEW: &str = "2026-08-10";

/// Rows `catalog.prices` should hold for a complete day. Counted by hand
/// from [`prices_json`] so the gate has a number that did not come from the
/// code under test: 5 + 1 + 1 for group 1, 2 + 1 + 1 for group 2.
pub const ROWS_PER_COMPLETE_DAY: usize = 11;

/// The single-card rows of that day — what `shared.prices` holds.
pub const SINGLE_ROWS_PER_DAY: usize = 7;

/// Two groups: one of single cards, one of sealed product.
const GROUPS: &str = r#"{"success":true,"errors":[],"results":[
    {"groupId":1,"name":"Base Set","abbreviation":"BS","publishedOn":"1999-01-09T00:00:00"},
    {"groupId":2,"name":"Base Set Sealed","abbreviation":"BSS","publishedOn":"1999-01-09T00:00:00"}
]}"#;

/// A single card carries a `Number` in `extendedData`; a sealed product does
/// not. That is the whole discriminator (`tcgcsv::is_single_card`), and it is
/// why the price payload alone cannot tell the two apart — which is the
/// difference the verifier has to account for.
fn products_json(group: i64) -> String {
    match group {
        1 => r#"{"success":true,"errors":[],"results":[
            {"productId":101,"groupId":1,"name":"Charizard - 4/102","imageCount":1,
             "extendedData":[{"name":"Number","value":"4/102"}]},
            {"productId":102,"groupId":1,"name":"Blastoise - 2/102","imageCount":1,
             "extendedData":[{"name":"Number","value":"2/102"}]}
        ]}"#
        .to_string(),
        _ => r#"{"success":true,"errors":[],"results":[
            {"productId":201,"groupId":2,"name":"Base Set Booster Box","imageCount":1,
             "extendedData":[]},
            {"productId":202,"groupId":2,"name":"Base Set Booster Pack","imageCount":1,
             "extendedData":[]}
        ]}"#
        .to_string(),
    }
}

/// Prices for one group on one "day". `day` shifts every quoted value, so a
/// table built from the wrong day's parts is wrong in its numbers and not
/// just its metadata.
///
/// The nulls are deliberate: a null is not a price and must produce no row.
/// So is product 202 being quoted under **two** sub-types — `sealed_prices`
/// is `UNIQUE(product, observed_at)` and has no `sub_type_name` column, so
/// SQLite keeps one of them and the lake keeps both.
fn prices_json(group: i64, day: usize) -> String {
    let d = day as f64;
    match group {
        1 => format!(
            r#"{{"success":true,"errors":[],"results":[
                {{"productId":101,"subTypeName":"Normal","lowPrice":{:.2},"midPrice":{:.2},
                  "highPrice":{:.2},"marketPrice":{:.2},"directLowPrice":{:.2}}},
                {{"productId":101,"subTypeName":"Holofoil","lowPrice":null,"midPrice":null,
                  "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}},
                {{"productId":102,"subTypeName":null,"lowPrice":null,"midPrice":null,
                  "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}}
            ]}}"#,
            10.0 + d,
            20.0 + d,
            30.0 + d,
            25.5 + d,
            24.0 + d,
            300.0 + d,
            5.25 + d,
        ),
        _ => format!(
            r#"{{"success":true,"errors":[],"results":[
                {{"productId":201,"subTypeName":"Normal","lowPrice":{:.2},"midPrice":null,
                  "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}},
                {{"productId":202,"subTypeName":"Normal","lowPrice":null,"midPrice":null,
                  "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}},
                {{"productId":202,"subTypeName":"Holofoil","lowPrice":null,"midPrice":null,
                  "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}}
            ]}}"#,
            4000.0 + d,
            4500.0 + d,
            120.0 + d,
            180.0 + d,
        ),
    }
}

/// What the upstream should do on the next pass: which day's prices to quote,
/// and which group's `/prices` to fail on (0 = none).
struct Script {
    day: AtomicUsize,
    fail_group: AtomicI64,
}

fn start_upstream(script: Arc<Script>) -> FakeUpstream {
    FakeUpstream::start(move |target, _n| {
        let parts: Vec<&str> = target.trim_matches('/').split('/').collect();
        match parts.as_slice() {
            ["3", "groups"] => Reply::ok(GROUPS),
            ["3", group, "products"] => Reply::ok(products_json(group.parse().unwrap())),
            ["3", group, "prices"] => {
                let group: i64 = group.parse().unwrap();
                if script.fail_group.load(Ordering::SeqCst) == group {
                    return Reply {
                        status: 503,
                        body: r#"{"error":"upstream is having a day"}"#.to_string(),
                    };
                }
                Reply::ok(prices_json(group, script.day.load(Ordering::SeqCst)))
            }
            _ => Reply {
                status: 404,
                body: format!(r#"{{"error":"no route for {target}"}}"#),
            },
        }
    })
}

/// One landing run: fetch every group's products and prices, land the bytes,
/// and import the *same* responses into `shared.sqlite`.
///
/// Both sides of the comparison come from one pass over one set of responses.
/// Fetching twice would leave "the lake and SQLite disagree" and "the upstream
/// answered differently" indistinguishable, which is precisely the confusion
/// the gate exists to remove.
fn land_run(
    base_url: &str,
    raw_root: &Path,
    db: &Path,
    ingest_date: &str,
    into_sqlite: bool,
) -> Option<String> {
    // The run's clock, the same one its rows are stamped with — recorded in
    // every manifest so a later derive reproduces those timestamps rather than
    // inventing its own. See `pkdump_derive::clock`.
    let now = format!("{ingest_date}T00:00:00Z");
    let landing = Arc::new(RawLanding::new(
        Box::new(DirStore::new(raw_root)),
        ingest_date,
        &now,
    ));
    let client = TcgcsvClient::new()
        .expect("client")
        .base_url(base_url)
        .landing_in(Arc::clone(&landing));

    let mut conn = pkdump_db::open_shared(db).expect("open shared catalog");

    let outcome = (|| -> Result<(), String> {
        let groups = client.fetch_groups().map_err(|e| e.to_string())?;
        if into_sqlite {
            tcgcsv::import_groups(&mut conn, &groups, &now).map_err(|e| e.to_string())?;
        }
        for group in &groups {
            let products = client
                .fetch_products(group.group_id)
                .map_err(|e| e.to_string())?;
            let prices = client
                .fetch_prices(group.group_id)
                .map_err(|e| e.to_string())?;
            if into_sqlite {
                tcgcsv::import_sealed_products(&mut conn, &products, &now)
                    .map_err(|e| e.to_string())?;
                tcgcsv::import_products(&mut conn, &products, &now).map_err(|e| e.to_string())?;
                tcgcsv::import_prices(&mut conn, &prices, ingest_date)
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })();

    let error = outcome.err();
    landing
        .finalize(error.as_deref())
        .expect("finalize the run's manifests");
    error
}

/// Where to build the fixture: `PKDUMP_PRICES_FIXTURE_OUT` if the caller
/// wants to keep it, otherwise a temp dir that goes away with the test.
fn out_dir() -> (PathBuf, Option<tempfile::TempDir>) {
    match std::env::var("PKDUMP_PRICES_FIXTURE_OUT") {
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

fn count(conn: &rusqlite::Connection, sql: &str, date: &str) -> i64 {
    conn.query_row(sql, [date], |r| r.get(0)).expect("count")
}

#[test]
fn builds_a_landing_zone_and_the_catalog_from_the_same_bytes() {
    let (out, _keep) = out_dir();
    let raw_root = out.join("raw-zone");
    let db = out.join("shared.sqlite");
    // A fixture directory is reused across runs; a stale DB would make the
    // row counts below depend on how many times the test has run.
    let _ = std::fs::remove_dir_all(&raw_root);
    let _ = std::fs::remove_file(&db);

    let script = Arc::new(Script {
        day: AtomicUsize::new(0),
        fail_group: AtomicI64::new(0),
    });
    let upstream = start_upstream(Arc::clone(&script));
    let base = upstream.base_url();

    // 1. An incomplete-only date: the run dies on group 2's prices.
    script.fail_group.store(2, Ordering::SeqCst);
    let failed = land_run(&base, &raw_root, &db, DATE_INCOMPLETE, false);
    assert!(
        failed.is_some_and(|e| e.contains("503")),
        "the incomplete date's run must fail on group 2"
    );

    // 2. A complete older date.
    script.fail_group.store(0, Ordering::SeqCst);
    script.day.store(1, Ordering::SeqCst);
    assert!(land_run(&base, &raw_root, &db, DATE_OLD, true).is_none());

    // 3. A complete newer date, then a retry that dies — with different
    //    prices again, so reading the wrong run is visible in the values.
    script.day.store(2, Ordering::SeqCst);
    assert!(land_run(&base, &raw_root, &db, DATE_NEW, true).is_none());
    script.day.store(9, Ordering::SeqCst);
    script.fail_group.store(2, Ordering::SeqCst);
    assert!(land_run(&base, &raw_root, &db, DATE_NEW, false).is_some());

    // -- what the landing zone holds ------------------------------------
    let runs = |date: &str| {
        let dir = raw_root.join(format!(
            "raw/source=tcgcsv/dataset=prices/ingest_date={date}"
        ));
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|e| e.expect("dir entry").file_name().to_string_lossy().into())
            .collect();
        names.sort();
        names
    };
    assert_eq!(runs(DATE_OLD).len(), 1);
    assert_eq!(
        runs(DATE_NEW).len(),
        2,
        "a retry must land BESIDE the first attempt, never on it"
    );

    let manifest = |date: &str, run: &str| -> pkdump_lake::Manifest {
        let key = pkdump_lake::keys::manifest_key(
            pkdump_lake::Source::Tcgcsv,
            pkdump_lake::Dataset::Prices,
            date,
            run.trim_start_matches("run="),
        );
        serde_json::from_slice(&std::fs::read(raw_root.join(key)).expect("manifest"))
            .expect("manifest parses")
    };

    let old = manifest(DATE_OLD, &runs(DATE_OLD)[0]);
    assert!(old.complete, "the older date's run finished");
    assert_eq!(old.parts.len(), 2, "one part per group");

    // By count, not by position: two runs a few milliseconds apart can share a
    // ULID timestamp, and which sorts first is then the random tail's business.
    let new_manifests: Vec<pkdump_lake::Manifest> = runs(DATE_NEW)
        .iter()
        .map(|run| manifest(DATE_NEW, run))
        .collect();
    let (done, partial): (Vec<_>, Vec<_>) = new_manifests.iter().partition(|m| m.complete);
    assert_eq!(done.len(), 1, "one run finished");
    assert_eq!(partial.len(), 1, "and one died");
    assert_eq!(done[0].parts.len(), 2);
    assert_eq!(
        partial[0].parts.len(),
        1,
        "the retry got group 1 and no further"
    );
    assert_eq!(partial[0].failures.len(), 1);

    let incomplete = manifest(DATE_INCOMPLETE, &runs(DATE_INCOMPLETE)[0]);
    assert!(!incomplete.complete);

    // -- what SQLite holds ----------------------------------------------
    let conn = rusqlite::Connection::open(&db).expect("open the built catalog");
    for date in [DATE_OLD, DATE_NEW] {
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM prices WHERE observed_at = ?1",
                date
            ),
            SINGLE_ROWS_PER_DAY as i64,
            "single-card price rows for {date}"
        );
        // Two sealed products, one row each — 202's second sub-type is the
        // one `UNIQUE(product, observed_at)` drops.
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM sealed_prices WHERE observed_at = ?1",
                date
            ),
            2,
            "sealed price rows for {date}"
        );
    }
    assert_eq!(
        conn.query_row("SELECT count(*) FROM sealed_products", [], |r| r
            .get::<_, i64>(0))
            .expect("sealed products"),
        2
    );

    // The prices really do differ by date — otherwise every "rebuilding an
    // older date reproduces that day" assertion downstream is vacuous.
    let market = |date: &str| -> f64 {
        conn.query_row(
            "SELECT price FROM prices WHERE tcgplayer_product_id = 101 \
             AND sub_type_name = 'Normal' AND price_type = 'market' AND observed_at = ?1",
            [date],
            |r| r.get(0),
        )
        .expect("market price")
    };
    assert_eq!(market(DATE_OLD), 26.5);
    assert_eq!(market(DATE_NEW), 27.5);

    if let Ok(path) = std::env::var("PKDUMP_PRICES_FIXTURE_OUT") {
        println!("fixture written to {path}");
    }
}
