//! The night is no longer lost when pokemontcg.io is down (pd-nons).
//!
//! On 2026-08-11 `api.pokemontcg.io` answered 500 or 502 to roughly 45% of
//! requests. `acquire` fetched its tail first and let the error propagate, so
//! one bad response on `/v2/sets?page=1` ended `pkdump data refresh` in its
//! first second — before TCGCSV was reached, so **no prices were imported at
//! all**. A day's prices cannot be re-fetched later; a day's set list can.
//!
//! The claim, which is not provable one crate down: **a tail that fails every
//! retry does not take the rest of the acquisition with it**, and still
//! reports itself as a failure rather than passing for a clean night.
//!
//! The tail deliberately still runs first. Moving TCGCSV in front of it —
//! perishable data first, the obvious companion change — was tried and
//! reverted: `import_groups` links each group to the `sets` rows already in
//! the database, so on a catalog the tail has not filled yet every link comes
//! out NULL and an offline rebuild stops matching the online catalog it
//! reproduces. `crates/pkdump-lakehouse/tests/row_identical.rs` is where that
//! shows up. See `acquire`'s docs.
//!
//! So what this asserts is ordering *around a failure*: TCGCSV is fetched
//! AFTER the tail has already given up. It runs against a fake upstream
//! standing in for BOTH origins — one server, so the request log is a single
//! ordered timeline rather than an inference from two clocks.

// The fake upstream lives in `pkdump-ingest`'s tests, where the clients it
// serves live. Included by path rather than copied: a second implementation
// of a test server is a second thing to keep honest, and the alternative — a
// crate in the workspace whose only job is to be a dev-dependency — is more
// scaffolding than the 100 lines it would hold.
#[path = "../../pkdump-ingest/tests/support/mod.rs"]
// Shared scaffolding: this binary uses Reply::status but not Reply::png, which only
// pkdump-lakehouse's row_identical.rs needs. CI runs clippy with -D warnings, so the
// unused half is a build failure without this. Same allow prices_fixture.rs carries.
#[allow(dead_code)]
mod support;

use std::path::Path;
use std::sync::Arc;

use pkdump_lake::{Dataset, DirStore, RawLanding, RawZone, Source};
use support::{FakeUpstream, Reply};

/// `derive` reads the origin and the retry budget from the environment,
/// which is process-wide. These tests set both, so they take turns.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serve both upstreams from one origin. TCGCSV lives under `/<category>/…`
/// and pokemontcg.io under `/sets` and `/cards`, so the paths never collide.
fn route(target: &str, tail: fn(&str) -> Reply) -> Reply {
    match target.split('?').next().unwrap_or(target) {
        "/3/groups" => Reply::ok(
            r#"{"results":[{"groupId":1,"name":"Base Set","abbreviation":"BS",
                            "publishedOn":"1999-01-09T00:00:00"}]}"#,
        ),
        // No Japanese groups: `japan::import_all` walks category 85 and this
        // gate is not about it.
        "/85/groups" => Reply::ok(r#"{"results":[]}"#),
        "/3/1/products" => Reply::ok(
            r#"{"results":[{"productId":100,"groupId":1,"name":"Charizard (Holo)",
                            "imageCount":1,
                            "extendedData":[{"name":"Number","displayName":"Number",
                                             "value":"4/102"}]}]}"#,
        ),
        // The perishable dataset. Its presence in the catalog afterwards is
        // what this whole test is about.
        "/3/1/prices" => Reply::ok(
            r#"{"results":[{"productId":100,"subTypeName":"Holofoil",
                            "lowPrice":100.0,"midPrice":250.0,"highPrice":900.0,
                            "marketPrice":312.5,"directLowPrice":275.0}]}"#,
        ),
        other => tail(other),
    }
}

/// Run one derivation against `upstream`, with `attempts` retries per URL and
/// no waiting between them.
fn derive_against(upstream: &FakeUpstream, dir: &Path, attempts: &str) -> pkdump_derive::Report {
    derive_landing(upstream, dir, attempts, None)
}

/// As [`derive_against`], landing every response into `landing` on the way past.
fn derive_landing(
    upstream: &FakeUpstream,
    dir: &Path,
    attempts: &str,
    landing: Option<Arc<RawLanding>>,
) -> pkdump_derive::Report {
    // SAFETY: ENV_LOCK is held by the caller for the whole of this call, and
    // nothing else in this binary touches the environment.
    unsafe {
        std::env::set_var("PKDUMP_TCGCSV_BASE_URL", upstream.base_url());
        std::env::set_var("PKDUMP_POKEMONTCG_BASE_URL", upstream.base_url());
        std::env::set_var("PKDUMP_HTTP_RETRY_ATTEMPTS", attempts);
        std::env::set_var("PKDUMP_HTTP_RETRY_BASE_MS", "1");
    }
    let mut conn = pkdump_db::open_shared(&dir.join("shared.sqlite")).unwrap();
    let report = pkdump_derive::derive(
        &mut conn,
        &pkdump_derive::Options {
            clock: pkdump_derive::DeriveClock::at(
                "2026-08-11T04:51:02Z".parse().expect("a fixed instant"),
            ),
            data_dir: dir,
            landing,
            replay: None,
        },
    )
    .expect("a derivation that acquired TCGCSV is not a failed derivation");
    unsafe {
        std::env::remove_var("PKDUMP_TCGCSV_BASE_URL");
        std::env::remove_var("PKDUMP_POKEMONTCG_BASE_URL");
        std::env::remove_var("PKDUMP_HTTP_RETRY_ATTEMPTS");
        std::env::remove_var("PKDUMP_HTTP_RETRY_BASE_MS");
    }
    report
}

fn price_rows(dir: &Path) -> i64 {
    let conn = pkdump_db::open_shared(&dir.join("shared.sqlite")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM prices", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_dead_pokemontcg_io_no_longer_costs_the_nights_prices() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let upstream =
        FakeUpstream::start(|target, _| route(target, |_| Reply::status(502, "bad gateway")));
    let tmp = tempfile::tempdir().unwrap();

    let report = derive_against(&upstream, tmp.path(), "3");

    // 1. The tail failed, and the run says so rather than pretending.
    let tail = report
        .tail_error
        .as_deref()
        .expect("a 502 on every attempt is a failed tail");
    assert!(tail.contains("502"), "{tail}");
    assert_eq!(report.sets_added, 0);

    // 2. And the prices landed anyway — the point of the whole change.
    assert_eq!(price_rows(tmp.path()), 5, "one row per price type");

    let served = upstream.requests();

    // 3. And it was fetched AFTER the tail had already failed — the whole
    //    claim, as an ordering fact rather than a row count. The old code
    //    reached this point by returning `Err` and never issuing a single
    //    TCGCSV request.
    let last_tail = served
        .iter()
        .rposition(|r| r.starts_with("/sets"))
        .expect("the tail was attempted");
    let first_tcgcsv = served
        .iter()
        .position(|r| r.starts_with("/3/"))
        .expect("TCGCSV was fetched");
    assert!(
        last_tail < first_tcgcsv,
        "TCGCSV must be fetched after the tail gave up, not instead of it: {served:?}"
    );

    // 4. The tail really did spend its retry budget before giving up.
    assert_eq!(
        served.iter().filter(|r| r.starts_with("/sets")).count(),
        3,
        "{served:?}"
    );
}

/// The ordinary night is unchanged: a healthy tail still imports its sets and
/// the run reports itself whole. `tail_error` must be a thing that happens
/// when the tail fails, not a thing that happens.
#[test]
fn a_healthy_tail_still_imports_its_sets() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let upstream = FakeUpstream::start(|target, _| {
        route(target, |t| {
            if t.starts_with("/sets") {
                Reply::ok(
                    r#"{"data":[{"id":"base1","name":"Base","series":"Base",
                                 "printedTotal":102,"total":102}]}"#,
                )
            } else {
                Reply::ok(r#"{"data":[]}"#)
            }
        })
    });
    let tmp = tempfile::tempdir().unwrap();

    let report = derive_against(&upstream, tmp.path(), "2");

    assert!(report.tail_error.is_none(), "{:?}", report.tail_error);
    assert_eq!(report.sets_added, 1);
    assert_eq!(price_rows(tmp.path()), 5);
}

/// The MANIFEST consequence of a dead tail, which is where pd-nons meets the
/// landing zone — and the thing neither test above could reach, both passing
/// `landing: None`.
///
/// `finalize` computes `complete` PER DATASET, and `acquire` deliberately does
/// not hand it the tail's error: the run was not cut short, only the tail was,
/// and every prefix written after it is whole. So the night this leaves behind
/// is a partition that is honest about which half is short —
///
///   pokemontcgio/sets   INCOMPLETE, carrying the 502 the retries ended on
///   tcgcsv/prices       complete, with the day's prices in it
///
/// — and that asymmetry is what `pkdump-lake-derive` reads to decide whether a
/// date is a partial night (exit 2) or an underivable one (exit 1). Passing
/// `landing: None` here for as long as we did is why nothing noticed the
/// derive was answering that night with a page (pd-llbq).
#[test]
fn a_dead_tail_leaves_a_partition_that_says_which_half_is_short() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let upstream =
        FakeUpstream::start(|target, _| route(target, |_| Reply::status(502, "bad gateway")));
    let tmp = tempfile::tempdir().unwrap();
    let raw = tmp.path().join("raw-zone");

    const DATE: &str = "2026-08-11";
    let landing = Arc::new(RawLanding::new(
        Box::new(DirStore::new(&raw)),
        DATE,
        "2026-08-11T04:51:02Z",
    ));
    let report = derive_landing(&upstream, tmp.path(), "2", Some(Arc::clone(&landing)));
    assert!(report.tail_error.is_some(), "the tail was supposed to fail");

    // Read back through the real reader rather than the writer's own state:
    // what the derive decides from is bytes in the zone, not an in-process
    // struct, and the two agreeing is part of the claim.
    let zone = RawZone::new(Box::new(DirStore::new(&raw)));
    let one = |source: Source, dataset: Dataset| {
        let runs = zone.runs(source, dataset, DATE).expect("list runs");
        assert_eq!(
            runs.len(),
            1,
            "{source}/{dataset}: one run landed this date"
        );
        runs.into_iter().next().unwrap()
    };

    // 1. The tail's own dataset is INCOMPLETE, and it says why.
    let sets = one(Source::PokemonTcgIo, Dataset::Sets);
    assert!(
        !sets.manifest.complete,
        "a failed tail is not a complete prefix"
    );
    assert_eq!(sets.manifest.parts.len(), 0, "a 502 lands no bytes");
    assert_eq!(
        sets.manifest.failures.len(),
        1,
        "{:?}",
        sets.manifest.failures
    );
    let failure = &sets.manifest.failures[0];
    assert_eq!(failure.status, Some(502));
    assert!(failure.url.contains("/sets?"), "{}", failure.url);

    // 2. …and only ONE failure, though the tail spent two attempts. A manifest
    //    failure means "this URL was not fetched", so recording the attempts a
    //    retry recovers from would mark a whole night incomplete for a hiccup
    //    it survived.
    assert_eq!(
        upstream
            .requests()
            .iter()
            .filter(|r| r.starts_with("/sets"))
            .count(),
        2,
        "the budget was spent"
    );

    // 3. The half a night cannot lose is COMPLETE, in the same run. This is the
    //    whole asymmetry: `finalize` was called with `None`, so completeness is
    //    each dataset's own answer rather than the run's.
    for dataset in [Dataset::Groups, Dataset::Products, Dataset::Prices] {
        let run = one(Source::Tcgcsv, dataset);
        assert!(
            run.manifest.complete,
            "tcgcsv/{dataset} must be complete on a night only the tail failed: {:?}",
            run.manifest.error
        );
        assert!(
            run.manifest.failures.is_empty(),
            "{:?}",
            run.manifest.failures
        );
        assert!(
            !run.manifest.parts.is_empty(),
            "tcgcsv/{dataset} landed nothing"
        );
    }

    // 4. One run, not two: the tail's failure did not start a new one.
    assert_eq!(
        one(Source::Tcgcsv, Dataset::Prices).run_id,
        sets.run_id,
        "every dataset of a night belongs to that night's run"
    );
}

// ---------------------------------------------------------------------------
// The same claim for the LANDING half (pd-lunn)
// ---------------------------------------------------------------------------

/// Run one LANDING pass against `upstream`, into a directory-backed lake.
///
/// The catalog is opened read-only, exactly as `pkdump data refresh` opens it
/// — so a landing run that started writing rows would fail here rather than on
/// prod's next morning.
fn land_against(
    upstream: &FakeUpstream,
    dir: &Path,
    attempts: &str,
) -> (pkdump_derive::Report, std::path::PathBuf) {
    // SAFETY: ENV_LOCK is held by the caller for the whole of this call, and
    // nothing else in this binary touches the environment.
    unsafe {
        std::env::set_var("PKDUMP_TCGCSV_BASE_URL", upstream.base_url());
        std::env::set_var("PKDUMP_POKEMONTCG_BASE_URL", upstream.base_url());
        std::env::set_var("PKDUMP_HTTP_RETRY_ATTEMPTS", attempts);
        std::env::set_var("PKDUMP_HTTP_RETRY_BASE_MS", "1");
    }
    let db = dir.join("shared.sqlite");
    // The catalog the refresh reads. Creating one is `pkdump setup`'s job;
    // this is that step.
    drop(pkdump_db::open_shared(&db).unwrap());

    let clock =
        pkdump_derive::DeriveClock::at("2026-08-11T04:51:02Z".parse().expect("a fixed instant"));
    let raw = dir.join("raw-zone");
    let landing = Arc::new(pkdump_lake::RawLanding::new(
        Box::new(pkdump_lake::DirStore::new(raw.clone())),
        clock.observed_date(),
        clock.fetched_at(),
    ));

    let conn = pkdump_db::open_shared_readonly(&db).unwrap();
    let report = pkdump_derive::land(
        &conn,
        &pkdump_derive::Options {
            clock,
            data_dir: dir,
            landing: Some(landing),
            replay: None,
        },
    )
    .expect("a landing run that landed TCGCSV is not a failed run");
    unsafe {
        std::env::remove_var("PKDUMP_TCGCSV_BASE_URL");
        std::env::remove_var("PKDUMP_POKEMONTCG_BASE_URL");
        std::env::remove_var("PKDUMP_HTTP_RETRY_ATTEMPTS");
        std::env::remove_var("PKDUMP_HTTP_RETRY_BASE_MS");
    }
    (report, raw)
}

/// Every part file under `raw`, by name.
fn landed_parts(raw: &Path) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path.to_string_lossy().into_owned());
            }
        }
    }
    let mut out = Vec::new();
    walk(raw, &mut out);
    out.sort();
    out
}

/// pd-nons's claim, restated for the command that exists now.
///
/// `pkdump data refresh` no longer derives, so "the night's prices" are not
/// rows in a catalog any more — they are bytes in `raw/`, and the catalog is
/// built from them hours later. A tail that dies must still leave the
/// perishable half of the day IN THE BUCKET, or the derive that follows has
/// nothing to build them from and the day is lost exactly as it was in
/// 2026-08-11.
#[test]
fn a_dead_pokemontcg_io_no_longer_costs_the_nights_landed_prices() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let upstream =
        FakeUpstream::start(|target, _| route(target, |_| Reply::status(502, "bad gateway")));
    let tmp = tempfile::tempdir().unwrap();

    let (report, raw) = land_against(&upstream, tmp.path(), "3");

    // 1. The tail failed, and the run says so rather than pretending.
    let tail = report
        .tail_error
        .as_deref()
        .expect("a 502 on every attempt is a failed tail");
    assert!(tail.contains("502"), "{tail}");
    assert_eq!(report.sets_added, 0);

    // 2. And the prices are in the bucket anyway — the point of the change,
    //    one layer further out than it used to be.
    let parts = landed_parts(&raw);
    assert!(
        parts.iter().any(|p| p.contains("dataset=prices")),
        "the perishable half must be landed even on a night the tail dies: {parts:?}"
    );

    // 3. The catalog was NOT touched. `land` took a read-only connection, so
    //    this is belt and braces — but it is the acceptance criterion for
    //    pd-lunn stated where a reader will look for it.
    let db = tmp.path().join("shared.sqlite");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let cards: i64 = conn
        .query_row("SELECT COUNT(*) FROM cards", [], |r| r.get(0))
        .unwrap();
    let prices: i64 = conn
        .query_row("SELECT COUNT(*) FROM prices", [], |r| r.get(0))
        .unwrap();
    assert_eq!((cards, prices), (0, 0), "the landing run derived a catalog");

    let served = upstream.requests();

    // 4. And TCGCSV was fetched AFTER the tail had already failed — the
    //    ordering claim, unchanged by the split.
    let last_tail = served
        .iter()
        .rposition(|r| r.starts_with("/sets"))
        .expect("the tail was attempted");
    let first_tcgcsv = served
        .iter()
        .position(|r| r.starts_with("/3/"))
        .expect("TCGCSV was fetched");
    assert!(
        last_tail < first_tcgcsv,
        "TCGCSV must be fetched after the tail gave up, not instead of it: {served:?}"
    );
}

/// The ordinary night for the landing half: a healthy tail lands its sets'
/// cards, and the catalog is still untouched.
#[test]
fn a_healthy_tail_lands_the_cards_of_the_sets_the_catalog_lacks() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let upstream = FakeUpstream::start(|target, _| {
        route(target, |t| {
            if t.starts_with("/sets") {
                Reply::ok(
                    r#"{"data":[{"id":"base1","name":"Base","series":"Base",
                                 "printedTotal":102,"total":102}]}"#,
                )
            } else {
                Reply::ok(r#"{"data":[]}"#)
            }
        })
    });
    let tmp = tempfile::tempdir().unwrap();

    let (report, raw) = land_against(&upstream, tmp.path(), "2");

    assert!(report.tail_error.is_none(), "{:?}", report.tail_error);
    assert_eq!(report.sets_added, 1, "one set the catalog does not have");

    // The cards endpoint was asked for, which is the whole of the tail's
    // catalog-dependent choice.
    let served = upstream.requests();
    assert!(
        served.iter().any(|r| r.starts_with("/cards")),
        "a set the catalog lacks must have its cards landed: {served:?}"
    );
    assert!(
        landed_parts(&raw)
            .iter()
            .any(|p| p.contains("source=pokemontcgio")),
        "and those bytes must be in the bucket"
    );

    let conn = rusqlite::Connection::open(tmp.path().join("shared.sqlite")).unwrap();
    let sets: i64 = conn
        .query_row("SELECT COUNT(*) FROM sets", [], |r| r.get(0))
        .unwrap();
    assert_eq!(sets, 0, "the landing run wrote a set row");
}
