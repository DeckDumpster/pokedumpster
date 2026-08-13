//! Item 3: a catalog derived from `raw/` is the catalog the online refresh
//! built from the responses `raw/` holds — **row by row**.
//!
//! Both sides of every comparison here come from one pass over one set of
//! upstream responses. The online side fetches them and lands them; the
//! offline side replays the bytes that landing kept. Fetching twice would make
//! "the two catalogs disagree" and "the upstream answered differently"
//! indistinguishable, which is the whole thing the gate exists to tell apart.
//!
//! ## What is real here and what is a stand-in
//!
//! | | |
//! | --- | --- |
//! | the derivation | real — [`pkdump_derive::derive`], the same function `pkdump data refresh` calls |
//! | the landing zone | real — `RawLanding` over a `DirStore`, the same writer prod uses over S3 |
//! | the reader | real — the shipped `pkdump-lake-derive` binary, run as a subprocess |
//! | the comparison | real — `pkdump-lake-derive diff`, the shipped comparator |
//! | the upstream | a fixture: an in-process HTTP server, so the run is hermetic |
//!
//! The offline half runs as a **subprocess of the real binary**, deliberately.
//! It is what a timer executes, its refusals are exit statuses rather than
//! `Result`s, and its loudness is text on a stream — none of which a test that
//! called an internal function would be checking.
//!
//! ## Two days, not one
//!
//! A single day would only ever build both catalogs from nothing, and the
//! interesting timestamps are the ones written on the *second* pass: day two
//! drops a TCGCSV product, which soft-deprecates a printing and stamps the
//! run's clock into `printings.deprecated_at`. That column used to be written
//! from `Utc::now()` inside variant expansion, which made it the one value an
//! offline rebuild could never reproduce. [`the_second_day_is_row_identical_too_deprecations_included`]
//! is what would catch that coming back.
//!
//! ## What is deliberately NOT proven here
//!
//! Egress. Nothing in this file can assert the network is unreachable — the
//! fixture upstream is a socket on loopback, and it must be, or the derive's
//! *fallback* could never be exercised at all. `tests/lake/derive.sh` is where
//! the derive runs on an `--internal` podman network with the
//! socket-to-1.1.1.1 assertion, against raw landed before that network
//! existed.

// The fake upstream is the one `pkdump-ingest`'s landing-zone gates already
// drive the real clients with. A second HTTP server would be a second thing
// that can be subtly wrong.
#[allow(dead_code)]
#[path = "../../pkdump-ingest/tests/support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use pkdump_derive::DeriveClock;
use pkdump_lake::{DirStore, RawLanding};
use support::{FakeUpstream, Reply};

/// The first day's partition, and the clock the run that landed it read.
const DAY1: &str = "2026-08-10";
const DAY1_CLOCK: &str = "2026-08-10T06:14:07+00:00";
/// The second day. Its products differ, so its rows must too.
const DAY2: &str = "2026-08-11";
const DAY2_CLOCK: &str = "2026-08-11T06:12:55+00:00";

/// `raw_derivation` is written by the offline job and by nothing else — the
/// online refresh fetched rather than replayed, so it has no run to name. It
/// is the one table that legitimately differs, and every comparison below
/// names it on the command line rather than skipping it quietly.
const PROVENANCE: &str = "raw_derivation";

/// The upstream's clients read their origin from the process environment, so
/// the online half of a comparison cannot run beside another one.
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// The fixture upstream
// ---------------------------------------------------------------------------

/// Two pokemontcg.io sets, bridged to TCGCSV groups by their `ptcgoCode`.
///
/// No `images` block: a set with no symbol URL is skipped by
/// `symbols::normalize_all_symbols`, which is the one phase of the derivation
/// that fetches something the landing zone deliberately does not hold. Leaving
/// it with nothing to do keeps this gate about the phases a replay CAN
/// reproduce; the gap itself is documented in `pkdump-derive`'s crate docs and
/// filed as pd-5w4n.
const SETS: &str = r#"{"data":[
  {"id":"fk1","name":"Fakemon Base","series":"Fakemon","printedTotal":2,"total":2,
   "ptcgoCode":"FK1","releaseDate":"2026/01/09"},
  {"id":"fk2","name":"Fakemon Jungle","series":"Fakemon","printedTotal":1,"total":1,
   "ptcgoCode":"FK2","releaseDate":"2026/06/16"}
],"page":1,"pageSize":250,"count":2,"totalCount":2}"#;

fn cards_json(set: &str) -> String {
    match set {
        "fk1" => r#"{"data":[
          {"id":"fk1-1","name":"Charizard","supertype":"Pokémon","subtypes":["Stage 2"],
           "hp":"120","types":["Fire"],"number":"1","rarity":"Rare Holo","artist":"Nobody",
           "nationalPokedexNumbers":[6]},
          {"id":"fk1-2","name":"Blastoise","supertype":"Pokémon","subtypes":["Stage 2"],
           "hp":"100","types":["Water"],"number":"2","rarity":"Rare Holo","artist":"Nobody",
           "nationalPokedexNumbers":[9]}
        ],"page":1,"pageSize":250,"count":2,"totalCount":2}"#
            .to_string(),
        _ => r#"{"data":[
          {"id":"fk2-1","name":"Snorlax","supertype":"Pokémon","subtypes":["Basic"],
           "hp":"90","types":["Colorless"],"number":"1","rarity":"Rare","artist":"Nobody",
           "nationalPokedexNumbers":[143]}
        ],"page":1,"pageSize":250,"count":1,"totalCount":1}"#
            .to_string(),
    }
}

/// English groups (category 3), each bridging to a set by abbreviation.
const GROUPS_EN: &str = r#"{"success":true,"errors":[],"results":[
  {"groupId":1,"name":"Fakemon Base","abbreviation":"FK1","publishedOn":"2026-01-09T00:00:00"},
  {"groupId":2,"name":"Fakemon Jungle","abbreviation":"FK2","publishedOn":"2026-06-16T00:00:00"}
]}"#;

/// One Japanese group (category 85). Japanese sets and cards are synthesized
/// from TCGCSV alone, so including one exercises a whole second acquisition
/// path — and proves its bytes replay from the same `tcgcsv/*` prefixes the
/// English pass lands under, keyed by URL rather than by category.
const GROUPS_JP: &str = r#"{"success":true,"errors":[],"results":[
  {"groupId":9001,"name":"FK1a: Fakemon Start Deck","abbreviation":"",
   "publishedOn":"2026-02-01T00:00:00"}
]}"#;

/// Products for one group on one day.
///
/// **On day 2, product 102 renumbers.** That is the point of the second day.
/// On day 1 it is the TCGCSV product for card `fk1-2`, priced as a reverse
/// holo, so expansion writes an `fk1-2-reverse_holo` printing. On day 2 its
/// collector number no longer matches any card, so expansion produces nothing
/// for that variant, soft-deprecates the row — and stamps the run's clock into
/// `printings.deprecated_at`.
///
/// That column is the reason the second day exists at all. It used to be
/// written from `Utc::now()` inside variant expansion, which made it the one
/// value an offline rebuild could never reproduce: the online run would stamp
/// the fetch, the offline one would stamp whenever the timer happened to fire,
/// and the two catalogs would differ in exactly one column of exactly one row.
fn products_json(group: i64, day: usize) -> String {
    match group {
        1 => {
            let blastoise_number = if day == 1 { "2/2" } else { "5/2" };
            format!(
                r#"{{"success":true,"errors":[],"results":[
                  {{"productId":101,"groupId":1,"name":"Charizard - 1/2","imageCount":1,
                    "extendedData":[{{"name":"Number","value":"1/2"}}]}},
                  {{"productId":102,"groupId":1,"name":"Blastoise - {blastoise_number}",
                    "imageCount":1,
                    "extendedData":[{{"name":"Number","value":"{blastoise_number}"}}]}}
                ]}}"#
            )
        }
        2 => r#"{"success":true,"errors":[],"results":[
              {"productId":201,"groupId":2,"name":"Snorlax - 1/1","imageCount":1,
               "extendedData":[{"name":"Number","value":"1/1"}]},
              {"productId":202,"groupId":2,"name":"Fakemon Jungle Booster Box","imageCount":1,
               "extendedData":[]}
            ]}"#
        .to_string(),
        _ => r#"{"success":true,"errors":[],"results":[
              {"productId":9101,"groupId":9001,"name":"Pikachu","imageCount":1,
               "extendedData":[{"name":"Number","value":"001/020"},
                               {"name":"CardType","value":"Pokemon"}]}
            ]}"#
        .to_string(),
    }
}

/// Prices for one group on one day. Every quoted value moves with the day, so
/// a catalog built from the wrong day's parts is wrong in its *numbers* and
/// not merely in its bookkeeping.
fn prices_json(group: i64, day: usize) -> String {
    let d = day as f64;
    match group {
        1 => format!(
            r#"{{"success":true,"errors":[],"results":[
              {{"productId":101,"subTypeName":"Holofoil","lowPrice":{:.2},"midPrice":{:.2},
                "highPrice":{:.2},"marketPrice":{:.2},"directLowPrice":null}},
              {{"productId":102,"subTypeName":"Reverse Holofoil","lowPrice":null,"midPrice":null,
                "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}}
            ]}}"#,
            10.0 + d,
            20.0 + d,
            30.0 + d,
            25.5 + d,
            5.25 + d,
        ),
        2 => format!(
            r#"{{"success":true,"errors":[],"results":[
              {{"productId":201,"subTypeName":"Normal","lowPrice":null,"midPrice":null,
                "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}},
              {{"productId":202,"subTypeName":"Normal","lowPrice":null,"midPrice":null,
                "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}}
            ]}}"#,
            3.5 + d,
            120.0 + d,
        ),
        _ => format!(
            r#"{{"success":true,"errors":[],"results":[
              {{"productId":9101,"subTypeName":"Normal","lowPrice":null,"midPrice":null,
                "highPrice":null,"marketPrice":{:.2},"directLowPrice":null}}
            ]}}"#,
            1.25 + d,
        ),
    }
}

/// The upstream both halves of a comparison read from. `day` selects which
/// day's payloads it serves; `fail_group` makes one `/prices` request return a
/// 503, which is how a run that dies partway is produced.
struct Script {
    day: AtomicUsize,
    fail_group: AtomicUsize,
}

fn start_upstream(script: Arc<Script>) -> FakeUpstream {
    FakeUpstream::start(move |target, _n| {
        let day = script.day.load(Ordering::SeqCst);
        let (path, _query) = target.split_once('?').unwrap_or((target, ""));

        if path == "/sets" {
            return Reply::ok(SETS);
        }
        if path == "/cards" {
            // reqwest percent-encodes `set.id:fk1`, so match on the id alone.
            let set = if target.contains("fk1") { "fk1" } else { "fk2" };
            return Reply::ok(cards_json(set));
        }

        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        match parts.as_slice() {
            ["3", "groups"] => Reply::ok(GROUPS_EN),
            ["85", "groups"] => Reply::ok(GROUPS_JP),
            [_, group, "products"] => Reply::ok(products_json(group.parse().unwrap(), day)),
            [_, group, "prices"] => {
                let group: usize = group.parse().unwrap();
                if script.fail_group.load(Ordering::SeqCst) == group {
                    return Reply {
                        status: 503,
                        body: r#"{"error":"upstream is having a day"}"#.to_string(),
                    };
                }
                Reply::ok(prices_json(group as i64, day))
            }
            _ => Reply {
                status: 404,
                body: format!(r#"{{"error":"no route for {target}"}}"#),
            },
        }
    })
}

// ---------------------------------------------------------------------------
// The two halves
// ---------------------------------------------------------------------------

/// A working directory with a landing zone, two catalogs and an upstream.
struct Harness {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    upstream: FakeUpstream,
    script: Arc<Script>,
}

impl Harness {
    fn start() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let script = Arc::new(Script {
            day: AtomicUsize::new(1),
            fail_group: AtomicUsize::new(0),
        });
        let upstream = start_upstream(Arc::clone(&script));
        Self {
            _tmp: tmp,
            root,
            upstream,
            script,
        }
    }

    fn raw(&self) -> PathBuf {
        self.root.join("raw-zone")
    }

    fn db(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.sqlite"))
    }

    fn day(&self, day: usize) {
        self.script.day.store(day, Ordering::SeqCst);
    }

    /// The ONLINE path: fetch from the fixture upstream, land every response,
    /// derive into `db`. This is `pkdump data refresh --land-raw` minus its
    /// argument parsing — the same `pkdump_derive::derive` call the CLI makes,
    /// with the same landing zone and the same clock.
    ///
    /// `fail` makes the acquisition phase die partway, which is how the
    /// incomplete partition the refusal test needs gets landed.
    fn online(&self, db: &Path, ingest_date: &str, clock_at: &str, fail: bool) -> bool {
        let _guard = self.env_lock();
        let clock = DeriveClock::from_manifest(clock_at, "the test's clock").expect("clock");
        let landing = Arc::new(RawLanding::new(
            Box::new(DirStore::new(self.raw())),
            ingest_date,
            clock.fetched_at(),
        ));
        self.script
            .fail_group
            .store(if fail { 2 } else { 0 }, Ordering::SeqCst);

        let mut conn = pkdump_db::open_shared(db).expect("open shared catalog");
        let outcome = pkdump_derive::derive(
            &mut conn,
            &pkdump_derive::Options {
                clock,
                data_dir: &self.root,
                landing: Some(Arc::clone(&landing)),
                replay: None,
            },
        );
        self.script.fail_group.store(0, Ordering::SeqCst);
        outcome.is_ok()
    }

    /// The OFFLINE job, as shipped: the real binary, its own process, its own
    /// environment. Returns its exit status and both streams.
    fn derive(&self, db: &Path, ingest_date: &str, extra: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pkdump-lake-derive"));
        cmd.args(["shared", "--ingest-date", ingest_date])
            .arg("--db")
            .arg(db)
            .arg("--data-dir")
            .arg(&self.root)
            .args(extra)
            .env("PKDUMP_LAKE_DIR", self.raw())
            // The landing zone is keyed by URL, so the replaying side has to
            // build the same URLs the fetching side did. In production that is
            // automatic — both are the compiled-in constant. Here the fixture
            // upstream's port is ephemeral, so it has to be passed along.
            .env("PKDUMP_TCGCSV_BASE_URL", self.upstream.base_url())
            .env("PKDUMP_POKEMONTCG_BASE_URL", self.upstream.base_url())
            // Never read: the lake is `PKDUMP_LAKE_DIR` above. Pointed at a
            // path that does not exist so a stray read of the operator's real
            // lake.env would fail loudly rather than pass silently.
            .env("PKDUMP_LAKE_ENV", self.root.join("no-such-lake.env"));
        cmd.output().expect("run pkdump-lake-derive")
    }

    /// `pkdump-lake-derive diff`, the shipped comparator, excluding only the
    /// provenance table the online path never writes.
    fn diff(&self, left: &Path, right: &Path) -> Output {
        self.diff_excluding(left, right, &[PROVENANCE])
    }

    /// As [`Harness::diff`], with the exclusions spelled out. Every caller
    /// that needs more than the provenance table has to say which and why —
    /// an exclusion nobody can see is how a comparison starts proving nothing.
    fn diff_excluding(&self, left: &Path, right: &Path, exclude: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_pkdump-lake-derive"));
        cmd.arg("diff")
            .arg("--left")
            .arg(left)
            .arg("--right")
            .arg(right);
        for table in exclude {
            cmd.args(["--exclude", table]);
        }
        cmd.output().expect("run pkdump-lake-derive diff")
    }

    fn env_lock(&self) -> MutexGuard<'_, ()> {
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: every writer of these variables in this binary holds the
        // lock above, and the offline half runs in its own process.
        unsafe {
            std::env::set_var("PKDUMP_TCGCSV_BASE_URL", self.upstream.base_url());
            std::env::set_var("PKDUMP_POKEMONTCG_BASE_URL", self.upstream.base_url());
        }
        guard
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A single value out of a catalog, for the assertions that are about one
/// specific row rather than about the whole file.
fn scalar<T: rusqlite::types::FromSql>(db: &Path, sql: &str) -> T {
    let conn = rusqlite::Connection::open(db).expect("open");
    conn.query_row(sql, [], |r| r.get(0)).expect("query")
}

// ---------------------------------------------------------------------------
// The gates
// ---------------------------------------------------------------------------

/// **The acceptance criterion.** Land a day through the online path, rebuild
/// it from what landed, and compare the two catalogs row for row.
#[test]
fn a_catalog_derived_from_raw_is_row_identical_to_the_online_one() {
    let h = Harness::start();
    let online = h.db("online");
    let offline = h.db("offline");

    assert!(h.online(&online, DAY1, DAY1_CLOCK, false), "online refresh");

    let out = h.derive(&offline, DAY1, &["--no-upstream-fallback"]);
    assert!(
        out.status.success(),
        "offline derive failed:\n{}",
        text(&out)
    );
    // With the fallback OFF, "it succeeded" already means every request was
    // answered from raw/ — a single miss would have been fatal. The line is
    // asserted as well, because it is what an operator reads.
    assert!(
        text(&out).contains("raw coverage: complete"),
        "the derive must SAY the partition covered it:\n{}",
        text(&out)
    );

    // Not an empty comparison: two empty catalogs are also row-identical.
    assert!(scalar::<i64>(&online, "SELECT COUNT(*) FROM cards") >= 3);
    assert!(scalar::<i64>(&online, "SELECT COUNT(*) FROM printings") > 0);
    assert!(scalar::<i64>(&online, "SELECT COUNT(*) FROM prices") > 0);
    assert!(scalar::<i64>(&online, "SELECT COUNT(*) FROM sealed_prices") > 0);

    let diff = h.diff(&online, &offline);
    assert!(
        diff.status.success(),
        "the two catalogs are not row-identical:\n{}",
        text(&diff)
    );
    assert!(text(&diff).contains("ROW-IDENTICAL"));
}

/// The same claim on the SECOND pass, where the incremental timestamps are.
///
/// Day two drops a TCGCSV product, so a printing is soft-deprecated and
/// `printings.deprecated_at` is written from the run's clock. This is the
/// assertion that would fail if that column went back to reading `Utc::now()`
/// inside variant expansion — the offline derive would stamp the moment it
/// ran, not the moment the bytes were fetched.
#[test]
fn the_second_day_is_row_identical_too_deprecations_included() {
    let h = Harness::start();
    let online = h.db("online");
    let offline = h.db("offline");

    h.day(1);
    assert!(h.online(&online, DAY1, DAY1_CLOCK, false), "day 1 online");
    assert!(
        h.derive(&offline, DAY1, &["--no-upstream-fallback"])
            .status
            .success(),
        "day 1 offline"
    );

    h.day(2);
    assert!(h.online(&online, DAY2, DAY2_CLOCK, false), "day 2 online");
    let out = h.derive(&offline, DAY2, &["--no-upstream-fallback"]);
    assert!(out.status.success(), "day 2 offline:\n{}", text(&out));

    // The row the second day exists for. Both catalogs must hold it, and both
    // must carry the instant the FETCH read rather than the one their own
    // derive ran at.
    let deprecations: Vec<(String, String)> = deprecated_printings(&online);
    assert!(
        !deprecations.is_empty(),
        "the fixture was supposed to deprecate a printing on day 2 — without one \
         this test proves nothing about `printings.deprecated_at`"
    );
    for (printing_id, at) in &deprecations {
        assert_eq!(
            at, DAY2_CLOCK,
            "{printing_id}: the deprecation must carry the clock the FETCH read"
        );
    }
    assert_eq!(
        deprecations,
        deprecated_printings(&offline),
        "the offline derive deprecated different rows, or at a different instant"
    );

    let diff = h.diff(&online, &offline);
    assert!(
        diff.status.success(),
        "day 2 is not row-identical:\n{}",
        text(&diff)
    );
}

/// Twice equals once, and the rerun is still identifiable.
///
/// Three things are asserted, and the first two pull in opposite directions on
/// purpose. Every table the app reads must be untouched by a second derive of
/// the same date — that is idempotence. `raw_derivation` must show that the
/// derive happened again, with the same run ids and no extra rows — that is
/// provenance. A design that got only the first would be indistinguishable
/// from one where the second derive silently did nothing.
///
/// The third is `set_aliases`, and it is the one exclusion here that is not
/// about this change at all. `open_shared` reconciles a layer of seed files on
/// every open, and the ones that FK into ingested rows write nothing until the
/// rows exist — the behaviour `catalog_prices` documents in
/// `pkdump-db/src/connection.rs`. So the seed lands on the *next* open, which
/// makes the first derive of a brand-new set differ from the second in that
/// table and no other. It is a property of opening the catalog rather than of
/// deriving it, it is identical on the online path, and it is filed rather than
/// fixed here (pd-zg7o).
///
/// It is also bounded, and the last assertion is what bounds it: a THIRD derive
/// matches the second with **nothing excluded but the provenance table**. One
/// step of convergence, then a fixed point.
#[test]
fn deriving_the_same_date_twice_changes_nothing_but_says_it_happened() {
    let h = Harness::start();
    let once = h.db("once");
    let twice = h.db("twice");

    assert!(h.online(&h.db("online"), DAY1, DAY1_CLOCK, false));
    assert!(
        h.derive(&twice, DAY1, &["--no-upstream-fallback"])
            .status
            .success()
    );

    // A byte copy of the catalog after ONE derive, to compare the second
    // derive's output against.
    std::fs::copy(&twice, &once).expect("snapshot the catalog");
    let first_derived_at: String = scalar(&once, "SELECT MIN(derived_at) FROM raw_derivation");
    let runs_before: i64 = scalar(&once, "SELECT COUNT(*) FROM raw_derivation");
    assert!(runs_before > 0, "the first derive recorded its provenance");

    assert!(
        h.derive(&twice, DAY1, &["--no-upstream-fallback"])
            .status
            .success()
    );

    let diff = h.diff_excluding(&once, &twice, &[PROVENANCE, "set_aliases"]);
    assert!(
        diff.status.success(),
        "a second derive of the same date changed the catalog:\n{}",
        text(&diff)
    );

    // …and the provenance says a derive ran again, without accumulating.
    assert_eq!(
        scalar::<i64>(&twice, "SELECT COUNT(*) FROM raw_derivation"),
        runs_before,
        "delete-then-insert per ingest_date — a rerun replaces, never appends"
    );
    assert_ne!(
        scalar::<String>(&twice, "SELECT MIN(derived_at) FROM raw_derivation"),
        first_derived_at,
        "a rerun must be IDENTIFIABLE, not invisible"
    );
    assert_eq!(
        scalar::<String>(&twice, "SELECT DISTINCT observed_at FROM raw_derivation"),
        DAY1,
        "the observation day comes from the run's clock"
    );

    // The fixed point: from the second derive on, nothing moves at all.
    let thrice = h.db("thrice");
    std::fs::copy(&twice, &thrice).expect("snapshot the catalog");
    assert!(
        h.derive(&thrice, DAY1, &["--no-upstream-fallback"])
            .status
            .success()
    );
    let diff = h.diff(&twice, &thrice);
    assert!(
        diff.status.success(),
        "the derive never reached a fixed point:\n{}",
        text(&diff)
    );
}

/// Rebuilding an OLDER date produces that date's catalog, not today's.
///
/// The failure this is aimed at is the reverse of a rerun: a job that reached
/// for the newest partition when asked for an older one would look like it
/// worked, and would quietly file the wrong day's prices.
#[test]
fn an_older_date_rebuilds_that_date_and_not_the_newest_one() {
    let h = Harness::start();
    let day1_only = h.db("day1-online");
    let both_days = h.db("both-online");

    // Land both days, and keep a catalog that only ever saw day one.
    h.day(1);
    assert!(h.online(&day1_only, DAY1, DAY1_CLOCK, false));
    std::fs::copy(&day1_only, h.db("day1-snapshot")).expect("snapshot");
    h.day(2);
    assert!(h.online(&both_days, DAY2, DAY2_CLOCK, false));

    // Now rebuild the OLDER date, with the newer one sitting in raw/ beside it.
    let rebuilt = h.db("rebuilt");
    let out = h.derive(&rebuilt, DAY1, &["--no-upstream-fallback"]);
    assert!(out.status.success(), "{}", text(&out));

    let diff = h.diff(&h.db("day1-snapshot"), &rebuilt);
    assert!(
        diff.status.success(),
        "rebuilding {DAY1} did not reproduce {DAY1}:\n{}",
        text(&diff)
    );
    // Stated directly as well: no row may carry the newer day's observation.
    assert_eq!(
        scalar::<i64>(
            &rebuilt,
            &format!("SELECT COUNT(*) FROM prices WHERE observed_at = '{DAY2}'")
        ),
        0,
        "a rebuild of {DAY1} must contain no {DAY2} prices"
    );
}

/// A date whose landing died partway is refused, not derived.
///
/// An incomplete run's parts are real bytes and an unknown fraction of the
/// day. Deriving from them would produce a catalog that is quietly smaller —
/// which, in a catalog, reads as *cards that do not exist*.
#[test]
fn an_incomplete_partition_is_refused_rather_than_half_derived() {
    let h = Harness::start();
    // Fails on group 2's prices, so the run lands part of the day and stops.
    assert!(
        !h.online(&h.db("online"), DAY1, DAY1_CLOCK, true),
        "the fixture upstream was supposed to fail this run"
    );

    let target = h.db("from-incomplete");
    let out = h.derive(&target, DAY1, &["--no-upstream-fallback"]);
    assert!(
        !out.status.success(),
        "an incomplete partition must not derive:\n{}",
        text(&out)
    );
    let said = text(&out);
    assert!(said.contains("no complete run"), "{said}");
    assert!(said.contains(DAY1), "{said}");
    // And it refused BEFORE writing anything: a refusal that left a partial
    // catalog behind would be the failure it exists to prevent, one step later.
    assert!(
        !target.exists(),
        "the refusal must not leave a catalog behind"
    );
}

/// The fallback is LOUD — asserted by exercising it, in both directions.
///
/// A URL is removed from the landing zone, which is what raw coverage
/// regressing looks like from the derive's side. With the fallback on the run
/// survives and must say, in as many words, that coverage has regressed. With
/// `--no-upstream-fallback` the same partition is a refusal. A gate that only
/// checked "it worked" would be green for a job that had quietly stopped using
/// the lake at all.
#[test]
fn a_gap_in_raw_is_loud_with_the_fallback_and_fatal_without_it() {
    let h = Harness::start();
    assert!(h.online(&h.db("online"), DAY1, DAY1_CLOCK, false));

    // Delete one landed payload — group 2's prices. The manifest still lists
    // it, so this is a lake that lost an object rather than a run that never
    // fetched one... which `payload`'s digest check would catch. Instead,
    // rewrite the manifest so the URL is genuinely absent from the partition:
    // the "we stopped landing this endpoint" shape.
    let manifest = find_manifest(&h.raw(), "dataset=prices");
    let text_before = std::fs::read_to_string(&manifest).expect("read manifest");
    let mut parsed: pkdump_lake::Manifest =
        serde_json::from_str(&text_before).expect("parse manifest");
    let dropped = parsed
        .parts
        .iter()
        .position(|p| p.url.contains("/2/prices"))
        .expect("group 2's prices are in the manifest");
    let dropped_url = parsed.parts.remove(dropped).url;
    std::fs::write(&manifest, parsed.to_json().expect("re-serialize")).expect("write manifest");

    // With the fallback ON: the run survives, by fetching. And says so.
    let out = h.derive(&h.db("with-fallback"), DAY1, &[]);
    assert!(
        out.status.success(),
        "the fallback should have carried this run:\n{}",
        text(&out)
    );
    let said = text(&out);
    assert!(
        said.contains("raw coverage has REGRESSED"),
        "the fallback must be LOUD, not merely successful:\n{said}"
    );
    assert!(said.contains(&dropped_url), "it must name the URL:\n{said}");

    // With it OFF — what item 4 makes unconditional — the same partition is a
    // refusal, and the message says what to do about it.
    let out = h.derive(&h.db("without-fallback"), DAY1, &["--no-upstream-fallback"]);
    assert!(
        !out.status.success(),
        "with the fallback off a gap must be fatal:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("raw/ has no record of"));
    assert!(text(&out).contains("--land-raw"));
}

/// A derivation that goes wrong must fail the job, not ship a smaller catalog.
///
/// The induced failure is a payload that no longer hashes to what its manifest
/// recorded — the shape a corrupted or tampered lake has. It is the case where
/// a silent wrong answer would be worst: the bytes still parse, the rows still
/// look like rows, and nothing downstream could tell.
#[test]
fn a_corrupted_payload_stops_the_derive_instead_of_deriving_from_it() {
    let h = Harness::start();
    assert!(h.online(&h.db("online"), DAY1, DAY1_CLOCK, false));

    let part = find_part(&h.raw(), "dataset=groups");
    let original = zstd::decode_all(&std::fs::read(&part).expect("read part")[..]).expect("unzstd");
    // Same length, different content: the length check must not be what
    // catches this, or the digest would be decoration.
    let mut tampered = original.clone();
    let last = tampered.len() - 3;
    tampered[last] = if tampered[last] == b'1' { b'2' } else { b'1' };
    assert_eq!(tampered.len(), original.len());
    std::fs::write(&part, zstd::encode_all(&tampered[..], 1).unwrap()).expect("write part");

    let target = h.db("from-corrupt");
    let out = h.derive(&target, DAY1, &["--no-upstream-fallback"]);
    assert!(
        !out.status.success(),
        "a payload that does not match its manifest must stop the derive:\n{}",
        text(&out)
    );
    assert!(text(&out).contains("sha256"), "{}", text(&out));
}

/// A date nobody landed is a refusal that names the date — never a rebuild
/// from whatever else the lake happens to hold.
#[test]
fn a_date_that_was_never_landed_refuses_by_name() {
    let h = Harness::start();
    assert!(h.online(&h.db("online"), DAY1, DAY1_CLOCK, false));

    let out = h.derive(&h.db("never"), "2001-01-01", &["--no-upstream-fallback"]);
    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("no runs landed"), "{said}");
    assert!(said.contains("2001-01-01"), "{said}");
    assert!(said.contains("never falls back"), "{said}");
}

/// There is no default `--ingest-date`. Deriving an older day is the same
/// operation as deriving today's, and a job that could read the clock would
/// have two behaviours where it should have one.
#[test]
fn the_ingest_date_has_no_default() {
    let out = Command::new(env!("CARGO_BIN_EXE_pkdump-lake-derive"))
        .args(["shared", "--db", "/nonexistent/shared.sqlite"])
        .output()
        .expect("run");
    assert!(!out.status.success());
    assert!(text(&out).contains("--ingest-date"), "{}", text(&out));
}

// ---------------------------------------------------------------------------

/// Every soft-deprecated printing and the instant it was deprecated at.
fn deprecated_printings(db: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(db).expect("open");
    let mut stmt = conn
        .prepare(
            "SELECT printing_id, deprecated_at FROM printings \
              WHERE deprecated_at IS NOT NULL ORDER BY printing_id",
        )
        .expect("prepare");
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query");
    rows.collect::<rusqlite::Result<_>>().expect("collect")
}

/// The first `_manifest.json` under a prefix containing `needle`.
fn find_manifest(raw: &Path, needle: &str) -> PathBuf {
    find(raw, needle, "_manifest.json")
}

/// The first landed part under a prefix containing `needle`.
fn find_part(raw: &Path, needle: &str) -> PathBuf {
    find(raw, needle, ".zst")
}

fn find(dir: &Path, needle: &str, suffix: &str) -> PathBuf {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.to_string_lossy().contains(needle)
                && path.to_string_lossy().ends_with(suffix)
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no {suffix} under a prefix matching {needle} in {dir:?}"))
}
