//! Bounded retry, through the real clients and a real socket (pd-nons).
//!
//! `crates/pkdump-ingest/src/retry.rs` proves the schedule and the
//! classification in isolation. What it cannot prove is the thing that
//! actually failed on 2026-08-11: that a transient 5xx on the way to
//! `api.pokemontcg.io` no longer ends a run — and that a 404 still does,
//! immediately, because asking a second time cannot change what a URL is.
//!
//! Every test here fixes its own budget with `.retry(...)` rather than
//! inheriting the default. A gate that slept the production backoff would
//! take 3.5s per failing URL to assert something about arithmetic that is
//! already unit-tested; what needs a socket is the *decision*, not the
//! duration. `the_backoff_actually_waits` is the one exception, and it uses
//! a budget small enough to measure without being slow.

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use pkdump_ingest::pokemontcg::PokemonTcgClient;
use pkdump_ingest::retry::RetryPolicy;
use pkdump_ingest::tcgcsv::TcgcsvClient;
use pkdump_lake::{Dataset, DirStore, Manifest, RawLanding, Source};
use support::{FakeUpstream, Reply};

const INGEST_DATE: &str = "2026-08-11";
const STARTED_AT: &str = "2026-08-11T04:51:02Z";

/// A budget with the production shape and none of its patience.
fn fast(attempts: u32) -> RetryPolicy {
    RetryPolicy::new(attempts, Duration::from_millis(1))
}

fn landing_in(dir: &std::path::Path) -> Arc<RawLanding> {
    Arc::new(RawLanding::new(
        Box::new(DirStore::new(dir)),
        INGEST_DATE,
        STARTED_AT,
    ))
}

fn manifest_of(
    root: &std::path::Path,
    landing: &RawLanding,
    source: Source,
    dataset: Dataset,
) -> Manifest {
    let key = pkdump_lake::keys::manifest_key(source, dataset, INGEST_DATE, landing.run_id());
    serde_json::from_slice(&std::fs::read(root.join(key)).expect("manifest on disk"))
        .expect("manifest parses")
}

fn down(status: u16) -> Reply {
    Reply {
        status,
        body: format!(r#"{{"error":"upstream says {status}"}}"#),
    }
}

const ONE_GROUP: &str = r#"{"results":[{"groupId":1,"name":"Base Set","abbreviation":"BS"}]}"#;

/// The bug, in one test: a single 5xx used to end the run. Now the second
/// attempt answers and the caller never learns there was a first.
#[test]
fn a_5xx_is_retried_and_the_next_attempt_wins() {
    let upstream = FakeUpstream::start(|_, n| match n {
        0 => down(500),
        _ => Reply::ok(ONE_GROUP),
    });

    let groups = TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .retry(fast(4))
        .fetch_groups()
        .expect("the retry answers");

    assert_eq!(groups.len(), 1);
    assert_eq!(upstream.requests().len(), 2, "one failure, one retry");
}

/// The one that was actually observed: `/v2/sets?page=1` 5xx-ing on the
/// pokemontcg.io client specifically. The whole refresh used to die here.
#[test]
fn the_pokemontcg_set_list_survives_a_flaky_start() {
    let upstream = FakeUpstream::start(|_, n| match n {
        0 | 1 => down(502),
        _ => Reply::ok(r#"{"data":[{"id":"sv3pt5","name":"151","series":"Scarlet & Violet"}]}"#),
    });

    let sets = PokemonTcgClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .retry(fast(4))
        .fetch_sets()
        .expect("the third attempt answers");

    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].id, "sv3pt5");
    assert_eq!(upstream.requests().len(), 3);
}

/// 429 is the upstream asking for a moment, not refusing the request.
#[test]
fn a_429_is_retried() {
    let upstream = FakeUpstream::start(|_, n| match n {
        0 => down(429),
        _ => Reply::ok(ONE_GROUP),
    });

    TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .retry(fast(3))
        .fetch_groups()
        .expect("the retry answers");

    assert_eq!(upstream.requests().len(), 2);
}

/// A 404 is a fact about the URL. Asking again is noise, and four times the
/// noise on a catalog-wide walk is a real cost.
#[test]
fn a_404_is_answered_once() {
    let upstream = FakeUpstream::start(|_, _| down(404));

    TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .retry(fast(4))
        .fetch_groups()
        .expect_err("404 propagates");

    assert_eq!(upstream.requests().len(), 1, "a 404 is not retried");
}

/// Bounded, and still loud at the end of it. This is the No-Fallback rule
/// holding: the retry buys attempts, not forgiveness.
#[test]
fn the_budget_runs_out_and_the_failure_propagates() {
    let upstream = FakeUpstream::start(|_, _| down(503));

    let err = TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .retry(fast(3))
        .fetch_groups()
        .expect_err("an upstream that never answers is still a failure");

    assert!(err.to_string().contains("503"), "{err}");
    assert_eq!(upstream.requests().len(), 3, "exactly the budget, no more");
}

/// One attempt, no waiting — the behaviour both clients had before pd-nons,
/// still expressible. `tests/lake/derive.sh` and friends rely on a fetch
/// failing fast rather than sleeping through a budget.
#[test]
fn a_client_with_no_budget_asks_once() {
    let upstream = FakeUpstream::start(|_, _| down(500));

    TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .retry(RetryPolicy::none())
        .fetch_groups()
        .expect_err("500 propagates");

    assert_eq!(upstream.requests().len(), 1);
}

/// It backs off rather than hammering. Four attempts at 40ms doubling is
/// 40 + 80 + 160 = 280ms of sleep; the assertion is a floor, so a slow
/// machine cannot fail it.
#[test]
fn the_backoff_actually_waits() {
    let upstream = FakeUpstream::start(|_, _| down(503));

    let started = Instant::now();
    TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .retry(RetryPolicy::new(4, Duration::from_millis(40)))
        .fetch_groups()
        .expect_err("503 throughout");
    let elapsed = started.elapsed();

    assert_eq!(upstream.requests().len(), 4);
    assert!(
        elapsed >= Duration::from_millis(280),
        "expected exponential backoff between attempts, took {elapsed:?}"
    );
}

/// A hiccup a retry recovered from must not mark the run incomplete.
///
/// `pkdump_lake::sink::finalize` computes `complete` from
/// `failures.is_empty()`, so recording every attempt would make one 500 on
/// one URL condemn a whole night's raw partition — and pd-up36's
/// age-based alarming reads exactly that flag. What the manifest records is
/// "this URL was not fetched", and after a successful retry it was.
#[test]
fn a_recovered_failure_is_not_recorded_in_the_manifest() {
    let upstream = FakeUpstream::start(|_, n| match n {
        0 => down(500),
        _ => Reply::ok(ONE_GROUP),
    });
    let tmp = tempfile::tempdir().unwrap();
    let landing = landing_in(tmp.path());

    TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .landing_in(Arc::clone(&landing))
        .retry(fast(4))
        .fetch_groups()
        .expect("the retry answers");
    landing.finalize(None).unwrap();

    let m = manifest_of(tmp.path(), &landing, Source::Tcgcsv, Dataset::Groups);
    assert!(m.complete, "a recovered hiccup leaves a complete manifest");
    assert!(m.failures.is_empty(), "{:?}", m.failures);
    assert_eq!(m.parts.len(), 1, "one part: the response that arrived");
    assert_eq!(m.parts[0].status, 200);
}

/// A URL that never answers records exactly one failure — the last attempt.
/// Four rows for one URL would read as four lost endpoints.
#[test]
fn an_exhausted_budget_records_one_failure_not_one_per_attempt() {
    let upstream = FakeUpstream::start(|_, _| down(503));
    let tmp = tempfile::tempdir().unwrap();
    let landing = landing_in(tmp.path());

    TcgcsvClient::new()
        .unwrap()
        .base_url(&upstream.base_url())
        .landing_in(Arc::clone(&landing))
        .retry(fast(3))
        .fetch_groups()
        .expect_err("503 throughout");

    let m = manifest_of(tmp.path(), &landing, Source::Tcgcsv, Dataset::Groups);
    assert_eq!(upstream.requests().len(), 3);
    assert_eq!(m.failures.len(), 1, "{:?}", m.failures);
    assert_eq!(m.failures[0].status, Some(503));
    assert!(m.failures[0].url.ends_with("/3/groups"));
    assert!(!m.complete);
    assert!(m.parts.is_empty());
}
