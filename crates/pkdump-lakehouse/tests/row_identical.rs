//! Item 3: a catalog derived from `raw/` is the catalog a FETCHING pass built
//! from the responses `raw/` holds — **row by row**.
//!
//! Both sides of every comparison here come from one pass over one set of
//! upstream responses. The fetching side fetches them and lands them; the
//! offline side replays the bytes that landing kept. Fetching twice would make
//! "the two catalogs disagree" and "the upstream answered differently"
//! indistinguishable, which is the whole thing the gate exists to tell apart.
//!
//! **The fetching side is this harness, not a shipped command** (pd-lunn).
//! `pkdump data refresh` used to fetch AND derive; item 6 split it, and
//! [`Harness::online`] is what is left of the half that built a catalog. What
//! ships is item 6's own gate,
//! [`a_catalog_derived_from_a_landing_only_refresh_is_row_identical_to_a_fetched_one`],
//! which runs the real landing path and measures its partition against this
//! one.
//!
//! ## What is real here and what is a stand-in
//!
//! | | |
//! | --- | --- |
//! | the derivation | real — [`pkdump_derive::derive`], the function `pkdump-lake-derive shared` calls |
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
//! ## The upstream stays UP, on purpose
//!
//! The fixture is a socket on loopback and it answers throughout every test
//! here, including the ones that assert a refusal. That is deliberate: a gap in
//! `raw/` is fatal by policy, and a gate run with nothing listening could not
//! tell policy from a failed connection. It is also what lets
//! [`a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it`] serve a
//! real set symbol — the one thing the offline derive still fetches.
//!
//! ## What is deliberately NOT proven here
//!
//! Egress. Nothing in this file can assert the network is unreachable, for the
//! reason just given. `tests/lake/derive.sh` is where the derive runs on an
//! `--internal` podman network with the socket-to-1.1.1.1 assertion, against
//! raw landed before that network existed.

// The fake upstream is the one `pkdump-ingest`'s landing-zone gates already
// drive the real clients with. A second HTTP server would be a second thing
// that can be subtly wrong.
#[allow(dead_code)]
#[path = "../../pkdump-ingest/tests/support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
/// With `symbols` `None` there is no `images` block, so a set carries no symbol
/// URL and `symbols::normalize_all_symbols` skips it — the one phase of the
/// derivation that fetches something the landing zone deliberately does not
/// hold, left with nothing to do. That keeps most of this file about the phases
/// a replay CAN reproduce.
///
/// With `symbols` `Some(origin)` both sets carry a real upstream symbol URL,
/// which is the **cold** shape: see
/// [`a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it`].
fn sets_json(symbols: Option<&str>) -> String {
    let images = |set: &str| match symbols {
        Some(origin) => format!(r#","images":{{"symbol":"{origin}/symbol/{set}.png"}}"#),
        None => String::new(),
    };
    format!(
        r#"{{"data":[
  {{"id":"fk1","name":"Fakemon Base","series":"Fakemon","printedTotal":2,"total":2,
   "ptcgoCode":"FK1","releaseDate":"2026/01/09"{}}},
  {{"id":"fk2","name":"Fakemon Jungle","series":"Fakemon","printedTotal":1,"total":1,
   "ptcgoCode":"FK2","releaseDate":"2026/06/16"{}}}
],"page":1,"pageSize":250,"count":2,"totalCount":2}}"#,
        images("fk1"),
        images("fk2"),
    )
}

/// A set symbol as `images.pokemontcg.io` serves one: a transparent canvas
/// with an opaque glyph somewhere in it, which is the shape the normalizer's
/// alpha-bbox trim exists for.
fn symbol_png() -> Vec<u8> {
    let mut img = image::RgbaImage::from_pixel(40, 40, image::Rgba([0, 0, 0, 0]));
    for y in 8..20 {
        for x in 5..25 {
            img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
        }
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("encode the fixture symbol");
    out
}

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
/// 503, which is how a run that dies partway is produced; `tail_down` makes
/// `/sets` answer 503 to everything, which is the OTHER failure shape and a
/// different one — see [`a_night_short_only_in_the_tail_derives_and_says_so`];
/// `symbols` is the origin set symbols are advertised under, filled in after
/// the listener binds because that is when the port is known.
struct Script {
    day: AtomicUsize,
    fail_group: AtomicUsize,
    tail_down: AtomicBool,
    symbols: Mutex<Option<String>>,
}

fn start_upstream(script: Arc<Script>) -> FakeUpstream {
    FakeUpstream::start(move |target, _n| {
        let day = script.day.load(Ordering::SeqCst);
        let (path, _query) = target.split_once('?').unwrap_or((target, ""));

        if path == "/sets" {
            // The 2026-08-11 shape: api.pokemontcg.io answering 5xx to
            // everything while TCGCSV is fine. Every retry gets this.
            if script.tail_down.load(Ordering::SeqCst) {
                return Reply::status(503, r#"{"error":"api.pokemontcg.io is having 2026-08-11"}"#);
            }
            let symbols = script.symbols.lock().expect("symbols lock").clone();
            return Reply::ok(sets_json(symbols.as_deref()));
        }
        // fk1's symbol is served; fk2's is a 404. One derive then exercises
        // both halves of the normalizer's outcome — a symbol that normalizes
        // and one whose fetch fails — without either being fatal.
        if path == "/symbol/fk1.png" {
            return Reply::png(symbol_png());
        }
        if path == "/symbol/fk2.png" {
            return Reply::status(404, r#"{"error":"no symbol for fk2"}"#);
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
                    return Reply::status(503, r#"{"error":"upstream is having a day"}"#);
                }
                Reply::ok(prices_json(group as i64, day))
            }
            _ => Reply::status(404, format!(r#"{{"error":"no route for {target}"}}"#)),
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
        Self::at(None)
    }

    /// A harness whose working directory is `out` rather than a temp dir —
    /// how `tests/lake/derive.sh` gets a landing zone produced by the REAL
    /// writer instead of a shell's idea of the key layout. The temp dir is
    /// still created and still cleans up; it is simply unused.
    fn at(out: Option<PathBuf>) -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = out.unwrap_or_else(|| tmp.path().to_path_buf());
        std::fs::create_dir_all(&root).expect("create the working directory");
        let script = Arc::new(Script {
            day: AtomicUsize::new(1),
            fail_group: AtomicUsize::new(0),
            tail_down: AtomicBool::new(false),
            symbols: Mutex::new(None),
        });
        let upstream = start_upstream(Arc::clone(&script));
        Self {
            _tmp: tmp,
            root,
            upstream,
            script,
        }
    }

    /// Advertise a set symbol URL on `/sets`, pointing back at this upstream.
    ///
    /// Only callable after the listener has bound, which is why it is a method
    /// rather than a constructor argument: the origin is the ephemeral port.
    fn serve_symbols(&self) {
        *self.script.symbols.lock().expect("symbols lock") = Some(self.upstream.base_url());
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

    /// Take `api.pokemontcg.io` down without touching TCGCSV.
    ///
    /// It affects the ONLINE half only. The offline job replays, and the URL a
    /// dead tail failed on is not in the partition, so its tail fails at that
    /// same request without a socket being opened — which is the property the
    /// partial night rests on.
    fn tail_down(&self, down: bool) {
        self.script.tail_down.store(down, Ordering::SeqCst);
    }

    /// The ONLINE path: fetch from the fixture upstream, land every response,
    /// derive into `db`. This is what `pkdump data refresh` was until pd-lunn
    /// split the two halves apart: one call that both landed and built. It is
    /// kept as the FETCHING half of every comparison here — the catalog a
    /// replay is measured against has to come from somewhere, and it has to
    /// come from the wire.
    ///
    /// `fail` makes the acquisition phase die partway, which is how the
    /// incomplete partition the refusal test needs gets landed.
    fn online(&self, db: &Path, ingest_date: &str, clock_at: &str, fail: bool) -> bool {
        let clock = DeriveClock::from_manifest(clock_at, "the test's clock").expect("clock");
        let landing = Arc::new(RawLanding::new(
            Box::new(DirStore::new(self.raw())),
            ingest_date,
            clock.fetched_at(),
        ));
        self.fetch_and_derive(db, clock, Some(landing), fail)
    }

    /// A derivation that fetches and lands NOTHING — the reference catalog in
    /// [`a_catalog_derived_from_a_landing_only_refresh_is_row_identical_to_a_fetched_one`].
    ///
    /// It exists because that gate's partition is landed by
    /// [`Harness::land`], and a second writer into the same `raw/` prefix
    /// would make the comparison one between two landings rather than one
    /// between a landing and a fetch.
    fn online_unlanded(&self, db: &Path, clock_at: &str) -> bool {
        let clock = DeriveClock::from_manifest(clock_at, "the test's clock").expect("clock");
        self.fetch_and_derive(db, clock, None, false)
    }

    fn fetch_and_derive(
        &self,
        db: &Path,
        clock: DeriveClock,
        landing: Option<Arc<RawLanding>>,
        fail: bool,
    ) -> bool {
        let _guard = self.env_lock();
        self.script
            .fail_group
            .store(if fail { 2 } else { 0 }, Ordering::SeqCst);

        let mut conn = pkdump_db::open_shared(db).expect("open shared catalog");
        let outcome = pkdump_derive::derive(
            &mut conn,
            &pkdump_derive::Options {
                clock,
                data_dir: &self.root,
                landing,
                replay: None,
            },
        );
        self.script.fail_group.store(0, Ordering::SeqCst);
        outcome.is_ok()
    }

    /// The ONLINE path as it is since pd-lunn: `pkdump data refresh` — fetch
    /// every upstream, land every response, and derive nothing at all.
    ///
    /// `db` must already exist; the catalog is opened READ-ONLY, which is how
    /// the CLI opens it and is the whole of the claim that a refresh writes no
    /// catalog table.
    fn land(&self, db: &Path, ingest_date: &str, clock_at: &str) -> pkdump_derive::Report {
        let _guard = self.env_lock();
        let clock = DeriveClock::from_manifest(clock_at, "the test's clock").expect("clock");
        let landing = Arc::new(RawLanding::new(
            Box::new(DirStore::new(self.raw())),
            ingest_date,
            clock.fetched_at(),
        ));
        let conn = pkdump_db::open_shared_readonly(db).expect("open the catalog read-only");
        pkdump_derive::land(
            &conn,
            &pkdump_derive::Options {
                clock,
                data_dir: &self.root,
                landing: Some(landing),
                replay: None,
            },
        )
        .expect("the landing run")
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

    let out = h.derive(&offline, DAY1, &[]);
    assert!(
        out.status.success(),
        "offline derive failed:\n{}",
        text(&out)
    );
    // "It succeeded" already means every request was answered from raw/ — a
    // single miss is fatal (item 4). The line is asserted as well, because it
    // is what an operator reads.
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

/// **The acceptance gate for item 6** (pd-lunn): the catalog has ONE builder.
///
/// [`a_catalog_derived_from_raw_is_row_identical_to_the_online_one`] above
/// proves that a replayed catalog equals a fetched one — but it lands with
/// `pkdump_derive::derive`, which also *builds* a catalog as it goes. That was
/// the shape of `pkdump data refresh` until item 6, and it is why
/// `pkdump-derive@` shipped disabled everywhere: arming it only rebuilt from
/// raw what the refresh had already built online.
///
/// The refresh now lands and nothing else, so the partition it leaves has to
/// stand on its own. This is that claim, end to end, with each half doing only
/// its own job:
///
/// 1. `pkdump_derive::land` fetches every upstream and lands it, over a
///    catalog opened READ-ONLY — which is the enforcement, not a convention.
///    The catalog is byte-identical afterwards.
/// 2. the shipped `pkdump-lake-derive` builds a catalog from that partition,
///    and says `raw coverage: complete` — every URL it asked for was there.
/// 3. that catalog is row-identical to one built by fetching the same upstream
///    with `derive`, which is the path this replaces.
///
/// Step 3 is what a URL the landing half stops asking for fails on: the derive
/// refuses a partition that does not answer it (item 4 deleted the fallback),
/// so the gate goes red here rather than on prod's next morning.
#[test]
fn a_catalog_derived_from_a_landing_only_refresh_is_row_identical_to_a_fetched_one() {
    let h = Harness::start();
    let read_by_refresh = h.db("read-by-refresh");
    let offline = h.db("offline");
    let reference = h.db("reference");

    // The catalog the refresh reads. In production this is the one the box is
    // already serving; creating it is `pkdump setup`'s job, and the refresh
    // refuses rather than creating one itself.
    drop(pkdump_db::open_shared(&read_by_refresh).expect("the catalog the refresh reads"));
    let before = std::fs::read(&read_by_refresh).expect("read the catalog");

    // 1. LAND. Every set is new to this catalog, so the tail's cards are
    //    fetched — which is the one URL choice that depends on what the
    //    catalog holds.
    let report = h.land(&read_by_refresh, DAY1, DAY1_CLOCK);
    assert!(
        report.tail_error.is_none(),
        "the fixture upstream answers everything: {:?}",
        report.tail_error
    );
    assert_eq!(report.sets_added, 2, "both fixture sets are new");

    // And it wrote no catalog table. Byte-identical, not "no rows I thought to
    // count" — the claim is about the file.
    assert_eq!(
        before,
        std::fs::read(&read_by_refresh).expect("re-read the catalog"),
        "the landing run modified the catalog it was only meant to read"
    );
    assert_eq!(
        scalar::<i64>(&read_by_refresh, "SELECT COUNT(*) FROM cards"),
        0,
        "the refresh derived cards"
    );
    assert_eq!(
        scalar::<i64>(&read_by_refresh, "SELECT COUNT(*) FROM prices"),
        0,
        "the refresh derived prices"
    );

    // 2. DERIVE, from that partition alone, with the shipped binary.
    let out = h.derive(&offline, DAY1, &[]);
    assert!(
        out.status.success(),
        "the partition a landing-only refresh left is not derivable:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("raw coverage: complete"),
        "a landing-only refresh must land every URL the derive asks for:\n{}",
        text(&out)
    );

    // 3. And it equals the catalog the deleted path would have built.
    assert!(
        h.online_unlanded(&reference, DAY1_CLOCK),
        "the reference derivation"
    );
    assert!(scalar::<i64>(&reference, "SELECT COUNT(*) FROM cards") >= 3);
    assert!(scalar::<i64>(&reference, "SELECT COUNT(*) FROM prices") > 0);
    assert!(scalar::<i64>(&reference, "SELECT COUNT(*) FROM sealed_prices") > 0);

    let diff = h.diff(&reference, &offline);
    assert!(
        diff.status.success(),
        "a catalog built from a landing-only refresh differs from a fetched one:\n{}",
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
        h.derive(&offline, DAY1, &[]).status.success(),
        "day 1 offline"
    );

    h.day(2);
    assert!(h.online(&online, DAY2, DAY2_CLOCK, false), "day 2 online");
    let out = h.derive(&offline, DAY2, &[]);
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
    assert!(h.derive(&twice, DAY1, &[]).status.success());

    // A byte copy of the catalog after ONE derive, to compare the second
    // derive's output against.
    std::fs::copy(&twice, &once).expect("snapshot the catalog");
    let first_derived_at: String = scalar(&once, "SELECT MIN(derived_at) FROM raw_derivation");
    let runs_before: i64 = scalar(&once, "SELECT COUNT(*) FROM raw_derivation");
    assert!(runs_before > 0, "the first derive recorded its provenance");

    assert!(h.derive(&twice, DAY1, &[]).status.success());

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
    assert!(h.derive(&thrice, DAY1, &[]).status.success());
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
    let out = h.derive(&rebuilt, DAY1, &[]);
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
/// pd-llbq: the night `api.pokemontcg.io` is down, end to end.
///
/// This is the OTHER failure shape, and the whole reason it needed its own
/// gate. [`an_incomplete_partition_is_refused_rather_than_half_derived`] above
/// kills a TCGCSV `/prices` request, which makes `acquire` return `Err`, ends
/// the run, and marks EVERY dataset of that run incomplete. A dead tail does
/// none of that: `acquire` carries the tail's error in `Report::tail_error`
/// (pd-nons) and every step after it runs to its end, so `finalize` is called
/// with `None` and the partition it leaves behind is honest per dataset —
/// `pokemontcgio/sets` short, `tcgcsv/prices` whole.
///
/// Nothing landed a partition of that shape and then tried to derive it, which
/// is how the two units came to answer one night's weather in opposite ways:
/// `pkdump data refresh` exits 2 and keeps the prices, and this job exited 1
/// and paged, on a partition it can perfectly well derive.
///
/// So: it derives, it is **row-identical to the catalog the online refresh
/// built on that same night**, and it says PARTIAL with exit 2.
#[test]
fn a_night_short_only_in_the_tail_derives_and_says_so() {
    let h = Harness::start();

    // A whole night first, so the partial one lands on a real catalog rather
    // than on nothing — two empty catalogs are also row-identical.
    h.day(1);
    assert!(h.online(&h.db("online"), DAY1, DAY1_CLOCK, false), "day 1");
    let target = h.db("derived");
    let out = h.derive(&target, DAY1, &[]);
    assert!(out.status.success(), "day 1 offline:\n{}", text(&out));

    // …then the night upstream is down. TCGCSV is untouched, so day 2's
    // PRICES — the half that cannot be re-fetched tomorrow — are fetched and
    // landed exactly as on any other night.
    h.day(2);
    h.tail_down(true);
    assert!(
        h.online(&h.db("online"), DAY2, DAY2_CLOCK, false),
        "a dead tail does not end the online run (pd-nons)"
    );

    // The partition really is short in the tail and whole everywhere else.
    // Read off the landed manifests, because that asymmetry is the input the
    // decision under test is made from.
    let sets = std::fs::read_to_string(find_manifest(
        &h.raw(),
        "dataset=sets/ingest_date=2026-08-11",
    ))
    .expect("the tail's manifest");
    assert!(sets.contains("\"complete\": false"), "{sets}");
    assert!(sets.contains("503"), "{sets}");
    let prices = std::fs::read_to_string(find_manifest(
        &h.raw(),
        "dataset=prices/ingest_date=2026-08-11",
    ))
    .expect("the prices manifest");
    assert!(prices.contains("\"complete\": true"), "{prices}");

    // The shipped binary, on that partition.
    let out = h.derive(&target, DAY2, &[]);
    let said = text(&out);
    assert_eq!(
        out.status.code(),
        Some(2),
        "a partial night is exit 2 — not 1 (a refusal) and not 0 (nothing to mention):\n{said}"
    );
    assert!(said.contains("PARTIAL PARTITION"), "{said}");
    assert!(said.contains("PARTIAL DERIVATION"), "{said}");
    assert!(said.contains("pokemontcgio/sets"), "{said}");
    // It must NOT claim complete raw coverage: the run asked for a URL the
    // partition does not hold. That line is read as a claim.
    assert!(!said.contains("raw coverage: complete"), "{said}");
    // …and it must not read as a coverage regression either. The partition
    // recorded that fetch FAILING, which is a different fact with different
    // advice — "re-land the date" cannot work for a night upstream was down.
    assert!(said.contains("the fetch FAILED"), "{said}");
    assert!(!said.contains("raw/ has no record of"), "{said}");

    // THE POINT: the night's prices are in the catalog. Refusing the partition
    // would have thrown them away, and there is no asking for them later —
    // which is pd-nons's whole argument, on the offline side of the split.
    assert!(
        scalar::<i64>(
            &target,
            "SELECT COUNT(*) FROM prices WHERE observed_at = '2026-08-11'"
        ) > 0,
        "the partial derive must still hold day two's prices"
    );

    // And it is the same catalog the online refresh built from the same bytes,
    // row for row — deprecations included, since day 2 renumbers a product.
    let cmp = h.diff(&h.db("online"), &target);
    assert!(
        cmp.status.success(),
        "a partial night must still be row-identical:\n{}",
        text(&cmp)
    );

    // Provenance IS written for a partial run, and it records which half was
    // short. A catalog with a stale set list and nothing saying so is the
    // quiet version of this failure.
    assert_eq!(
        scalar::<i64>(
            &target,
            "SELECT COUNT(*) FROM raw_derivation \
             WHERE ingest_date = '2026-08-11' AND dataset = 'sets' AND complete = 0"
        ),
        1,
        "the tail's dataset is recorded incomplete"
    );
    assert_eq!(
        scalar::<i64>(
            &target,
            "SELECT COUNT(*) FROM raw_derivation \
             WHERE ingest_date = '2026-08-11' AND source = 'tcgcsv' AND complete = 0"
        ),
        0,
        "the half a night cannot lose is recorded whole"
    );
}

/// A run cut short in TCGCSV. The exemption above is the TAIL's, and only the
/// tail's: `products` with 200 groups of an unknown 450 is precisely the
/// quietly-smaller catalog the refusal exists for, and it is the half of a
/// night that no later run can re-fetch.
#[test]
fn an_incomplete_partition_is_refused_rather_than_half_derived() {
    let h = Harness::start();
    // Fails on group 2's prices, so the run lands part of the day and stops.
    assert!(
        !h.online(&h.db("online"), DAY1, DAY1_CLOCK, true),
        "the fixture upstream was supposed to fail this run"
    );

    let target = h.db("from-incomplete");
    let out = h.derive(&target, DAY1, &[]);
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

/// A gap in `raw/` is FATAL, and it is fatal by default.
///
/// A URL is removed from the landing zone, which is what raw coverage
/// regressing looks like from the derive's side: the derivation grew an input
/// the landing zone does not capture, or an upstream's origin moved. The run
/// must stop there and say what to do about it.
///
/// This test used to assert the temporary fallback in both directions — loud
/// with it on, fatal with `--no-upstream-fallback`. Item 4 deleted the
/// fallback, so there is one direction left, and the extra assertion below is
/// what makes that stick: **the upstream is up and reachable throughout**. The
/// fixture is a socket on loopback that would have answered. A future edit that
/// reintroduced a fallback would make this run succeed rather than merely
/// change its wording, so the test would fail rather than rot.
///
/// The flag itself is asserted gone too. Leaving it accepted-but-ignored would
/// be worse than removing it: `deploy/derive.sh` invocations and runbooks
/// carrying it would keep passing while meaning nothing.
#[test]
fn a_gap_in_raw_is_fatal_with_the_upstream_sitting_right_there() {
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

    let target = h.db("gap");
    let out = h.derive(&target, DAY1, &[]);
    assert!(
        !out.status.success(),
        "a gap in raw/ must be fatal, with no flag asked for:\n{}",
        text(&out)
    );
    let said = text(&out);
    assert!(said.contains("raw/ has no record of"), "{said}");
    assert!(said.contains(&dropped_url), "it must name the URL:\n{said}");
    assert!(said.contains("pkdump data refresh"), "{said}");

    // Not "the network happened to be down". The fixture upstream is on
    // loopback and answering — the run before this one fetched every one of
    // these URLs from it. The refusal is policy, not circumstance.
    assert!(
        h.upstream
            .requests()
            .iter()
            .any(|r| r.contains("/2/prices")),
        "the fixture upstream never served group 2's prices, so this test would \
         pass against a fallback that simply could not reach anything"
    );

    // And the opt-out is gone rather than tolerated.
    let out = h.derive(&h.db("flag"), DAY1, &["--no-upstream-fallback"]);
    assert!(
        !out.status.success(),
        "--no-upstream-fallback must be rejected, not silently accepted:\n{}",
        text(&out)
    );
    assert!(
        text(&out).contains("unexpected argument"),
        "clap should refuse the retired flag by name:\n{}",
        text(&out)
    );
}

/// **The COLD derive** — sets carrying genuine upstream symbol URLs, with no
/// local PNGs and no `symbol_source_url` yet. A miss in `raw/` is fatal, and a
/// set symbol is not in `raw/` and never will be. This is the gate that says
/// that is fine.
///
/// It is here because the proof that let the fallback be deleted (pd-vves)
/// could not exercise it. Both halves of that comparison ran from a snapshot of
/// prod's LIVE catalog, whose `sets.symbol_url` had already been normalised to
/// `/sym/<set>.png` by earlier online refreshes — and
/// `normalize_all_symbols` skips a row that does not start with `http`. The
/// `sets` table matched because BOTH sides skipped the phase entirely, not
/// because the phase ran and reproduced. pd-5w4n named that gap; nothing
/// covered it until this test.
///
/// What it establishes, and why deleting the fallback cannot break it:
/// `normalize_all_symbols` takes `(&mut Connection, &Path)`. There is no `Wire`
/// in its signature, it imports nothing from `pkdump_ingest::landing`, and it
/// builds its own `reqwest::blocking::Client`. `ReplaySource::missing` — the
/// one thing that turns an absence from `raw/` into a refusal — is called from
/// exactly one place, `landing::fetch_bytes`, which the symbol phase does not
/// go through. So a symbol fetch is not a replay miss; it is a fetch, on a
/// phase that was never replayable and never claimed to be.
///
/// Two outcomes in one run, because both matter and neither may be fatal:
/// fk1's symbol is served and normalises, fk2's 404s and is counted.
///
/// ## What that does and does not prove about a box with no egress (pd-ju9c)
///
/// This sentence used to read "a box with no egress — `tests/lake/derive.sh`'s
/// `--internal` network, or prod on a night `images.pokemontcg.io` is down —
/// takes fk2's path for every set", as though a gate covered it. None does.
///
/// - What IS covered, here, over loopback: a set carrying a genuine `http`
///   symbol URL, a fetch that 404s, `failed` counted, the row left with its
///   upstream URL, and the derive not refused for any of it.
/// - What is NOT: a fetch that never reaches a server. The container gate does
///   not close that gap either — its fixture
///   ([`the_fixture_the_container_gate_reads`]) never calls `serve_symbols`, so
///   no set carries a symbol URL and `normalize_all_symbols` continues past
///   every row. On the `--internal` network the phase is SKIPPED, not
///   exercised-and-failed, and the gate's log carries no symbol line to say so.
///
/// A 404 and a connect refusal do reach the same `error_for_status()?` arm, so
/// the behaviour is almost certainly identical — but "almost certainly" is a
/// judgement, and the epic's own lesson is that a green result over a phase
/// that never executed is the thing to name rather than round off.
#[test]
fn a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it() {
    let h = Harness::start();
    h.serve_symbols();
    let online = h.db("online");
    let offline = h.db("offline");

    assert!(h.online(&online, DAY1, DAY1_CLOCK, false), "online refresh");
    // The online run normalised: fk1 now points at the local cache and records
    // where it came from, fk2 kept the upstream URL its fetch failed on.
    let symbol_url = format!("{}/symbol/fk1.png", h.upstream.base_url());
    assert_eq!(
        scalar::<String>(
            &online,
            "SELECT symbol_url FROM sets WHERE set_code = 'fk1'"
        ),
        "/sym/fk1.png"
    );
    assert_eq!(
        scalar::<String>(
            &online,
            "SELECT symbol_source_url FROM sets WHERE set_code = 'fk1'"
        ),
        symbol_url
    );
    assert_eq!(
        scalar::<String>(
            &online,
            "SELECT symbol_url FROM sets WHERE set_code = 'fk2'"
        ),
        format!("{}/symbol/fk2.png", h.upstream.base_url()),
        "a symbol whose fetch failed keeps its upstream URL, which still renders"
    );

    // The offline catalog is EMPTY, so its sets arrive from the replayed
    // `/sets` response carrying http symbol URLs and no `symbol_source_url` —
    // the cold shape, which is the whole point. `symbols/fk1.png` already
    // exists in the shared data dir from the online run and is deliberately
    // NOT enough to skip the fetch: the cache check is keyed on
    // `symbol_source_url`, which is NULL here.
    let before = h.upstream.requests().len();
    let out = h.derive(&offline, DAY1, &[]);
    assert!(
        out.status.success(),
        "a cold derive must not be refused over a set symbol:\n{}",
        text(&out)
    );
    let said = text(&out);

    // THE assertion. A symbol fetch that had gone through the replay layer
    // would have been a miss, and a miss is fatal — so this line and a live
    // symbol fetch in the same run is the proof that the phase bypasses it.
    assert!(
        said.contains("raw coverage: complete"),
        "a live symbol fetch must not count as a gap in raw/:\n{said}"
    );
    // fk1 normalised, fk2's fetch 404'd, and the one override is `mep` —
    // `tcgcsv::import_groups` synthesizes that set row from the compiled-in
    // bridge overlay with `symbol_url = /sets/mep-symbol.svg`, which does not
    // start with `http` and so is skipped rather than fetched. Nothing was
    // cached: the shared data dir already holds `symbols/fk1.png` from the
    // online run, and it is deliberately not enough — the cache check is keyed
    // on `symbol_source_url`, which is NULL on this cold catalog.
    assert!(
        said.contains("1 processed, 0 cached, 1 overrides, 1 failed"),
        "the symbol phase must have RUN — one normalised, one failed:\n{said}"
    );
    assert!(
        said.contains("images are deliberately not landed"),
        "a failed symbol must still explain itself to an operator:\n{said}"
    );

    // Not inferred from a log line: the fixture upstream was actually asked.
    let asked: Vec<String> = h.upstream.requests().split_off(before);
    assert!(
        asked.iter().any(|r| r == "/symbol/fk1.png"),
        "the offline derive never fetched fk1's symbol: {asked:?}"
    );

    // And the cold derive reproduced the online catalog's `sets` rows, which
    // is what pd-vves's proof could only assert about two skipped phases.
    assert_eq!(
        scalar::<String>(
            &offline,
            "SELECT symbol_url FROM sets WHERE set_code = 'fk1'"
        ),
        "/sym/fk1.png"
    );
    assert_eq!(
        scalar::<String>(
            &offline,
            "SELECT symbol_source_url FROM sets WHERE set_code = 'fk1'"
        ),
        symbol_url
    );
    let diff = h.diff(&online, &offline);
    assert!(
        diff.status.success(),
        "a cold derive is not row-identical:\n{}",
        text(&diff)
    );
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
    let out = h.derive(&target, DAY1, &[]);
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

    let out = h.derive(&h.db("never"), "2001-01-01", &[]);
    assert!(!out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("no runs landed"), "{said}");
    assert!(said.contains("2001-01-01"), "{said}");
    assert!(said.contains("never falls back"), "{said}");
}

/// The fixture `tests/lake/derive.sh` reads, built by the real client through
/// the real landing zone.
///
/// Hand-writing zstd parts and a manifest in the shell would be easier and
/// would prove nothing — it would assert that the reader can read *the
/// shell's* idea of the layout. Change the key layout, the manifest fields or
/// the zstd level and the container gate downstream notices.
///
/// Run bare it builds into a temp dir and asserts what it built. Point
/// `PKDUMP_DERIVE_FIXTURE_OUT` at a directory and it writes there instead:
///
/// ```sh
/// PKDUMP_DERIVE_FIXTURE_OUT=/tmp/fixture \
///   cargo test -p pkdump-lakehouse --test row_identical the_fixture
/// ```
///
/// It emits two days, the catalog each day's online refresh produced, and the
/// fixture upstream's origin. That last file is the awkward one and it is
/// honest: `raw/` is keyed by URL, so a replay has to build the same URLs the
/// fetch did. In production that is automatic — both sides are the compiled-in
/// constant — but a fixture upstream's port is ephemeral, so the container gate
/// has to be told what it was.
#[test]
fn the_fixture_the_container_gate_reads() {
    let out = std::env::var("PKDUMP_DERIVE_FIXTURE_OUT")
        .ok()
        .map(PathBuf::from);
    let keep = out.is_some();
    let h = Harness::at(out);

    h.day(1);
    assert!(h.online(&h.db("day1"), DAY1, DAY1_CLOCK, false), "day 1");
    std::fs::copy(h.db("day1"), h.db("online")).expect("carry day 1 forward");
    h.day(2);
    assert!(h.online(&h.db("online"), DAY2, DAY2_CLOCK, false), "day 2");
    std::fs::write(h.root.join("upstream-origin.txt"), h.upstream.base_url())
        .expect("record the fixture upstream's origin");

    // Both dates landed, each by one complete run.
    for date in [DAY1, DAY2] {
        assert!(
            h.raw()
                .join(format!(
                    "raw/source=tcgcsv/dataset=prices/ingest_date={date}"
                ))
                .exists(),
            "{date} landed no prices"
        );
    }
    assert!(scalar::<i64>(&h.db("day1"), "SELECT COUNT(*) FROM prices") > 0);

    if keep {
        println!("fixture written to {}", h.root.display());
    }
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
