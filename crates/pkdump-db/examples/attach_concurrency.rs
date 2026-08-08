//! Measurement harness: N tenants reading the shared catalog through
//! `ATTACH`, concurrently.
//!
//! Written for bead `pd-jgd4` (epic `pd-gckl`, per-tenant data model). The
//! epic's premise is that the catalog stays ONE file, `ATTACH`ed read-only
//! per tenant connection exactly as it is today, and that this stays cheap
//! as the number of simultaneous readers grows from one to several. That was
//! an assumption. This binary turns it into numbers. The write-up lives in
//! `deep-dives/attach-concurrency/`.
//!
//! # What it measures, and why in this shape
//!
//! The harness reproduces the server's concurrency structure rather than
//! inventing one. `pkdump_server::tenant::Tenants` keeps **one connection per
//! tenant, each behind its own `Mutex`, for the life of the process**, and
//! `blocking()` hands work to `spawn_blocking`. So: one long-lived
//! [`Connection`] per tenant, one thread per tenant, and the real
//! [`pkdump_db::binder::get_binder_page`] — the heaviest catalog-joining read
//! path there is — as the workload.
//!
//! Four scenarios, and the comparison between the first two is the whole
//! point:
//!
//! * `shared` — N tenants, ONE catalog file. What the epic proposes.
//! * `private` — N tenants, each `ATTACH`ing its OWN copy of the catalog.
//!   The **control**. A box with 4 cores cannot run 16 readers without
//!   latency rising, and that rise is CPU saturation, not SQLite. Sharing is
//!   free exactly to the extent that `shared` matches `private` at the same
//!   N. Without this arm the numbers cannot tell the two apart.
//! * `same_tenant` — N workers contending for ONE tenant's connection. This
//!   is the thing that *does* serialise, by design, and measuring it is how
//!   the claim "tenants do not serialise against each other" gets a
//!   counter-example to stand against.
//! * `refresh` — N readers plus a writer committing to the shared catalog in
//!   a loop, modelling `pkdump data refresh` running from its nightly timer
//!   while the server is up. Also records WAL growth, since continuous
//!   readers can hold a checkpoint off.
//!
//! # Running it
//!
//! Prefer `deep-dives/attach-concurrency/run.sh`, which picks sane sizes and
//! renders the table. Directly:
//!
//! ```text
//! cargo run --release --example attach_concurrency -- \
//!     --work /var/tmp/pd-attach --seconds 5 --levels 1,2,4,8
//! ```
//!
//! It writes only inside `--work` (a directory it creates) and touches no
//! `$PKDUMP_HOME`, no deployment and no real catalog: the catalog it reads is
//! one it generates. Generation is deterministic — same flags, same bytes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use pkdump_db::binder::{BinderQuery, get_binder_page};

// ---------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------

struct Config {
    work: PathBuf,
    sets: usize,
    cards_per_set: usize,
    owned_per_tenant: usize,
    seconds: u64,
    /// How many times the whole sweep repeats. Latencies pool across
    /// repetitions; more repetitions is the only honest way to get a usable
    /// p95 out of a workload where one operation costs ~100ms.
    reps: usize,
    levels: Vec<usize>,
    /// Bytes of filler per card row. Real catalog rows carry the upstream
    /// `raw_json`; row width decides how many pages a set scan touches, so a
    /// harness with skinny rows would flatter the shared arm.
    card_filler: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            work: PathBuf::from("/var/tmp/pd-attach-concurrency"),
            // ~200 sets x ~220 cards averages ~44k cards, the order of the
            // real catalog once the Japanese half is counted.
            sets: 200,
            cards_per_set: 220,
            owned_per_tenant: 3000,
            seconds: 8,
            reps: 3,
            levels: vec![1, 2, 4, 8],
            card_filler: 1200,
        }
    }
}

fn parse_args() -> Config {
    let mut cfg = Config::default();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| panic!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--work" => cfg.work = PathBuf::from(value()),
            "--sets" => cfg.sets = value().parse().expect("--sets"),
            "--cards-per-set" => cfg.cards_per_set = value().parse().expect("--cards-per-set"),
            "--owned" => cfg.owned_per_tenant = value().parse().expect("--owned"),
            "--seconds" => cfg.seconds = value().parse().expect("--seconds"),
            "--reps" => cfg.reps = value().parse().expect("--reps"),
            "--card-filler" => cfg.card_filler = value().parse().expect("--card-filler"),
            "--levels" => {
                cfg.levels = value()
                    .split(',')
                    .map(|s| s.trim().parse().expect("--levels"))
                    .collect()
            }
            other => panic!("unknown flag {other}"),
        }
    }
    assert!(!cfg.levels.is_empty(), "--levels is empty");
    cfg
}

fn main() {
    let cfg = parse_args();
    let max_readers = *cfg.levels.iter().max().unwrap();

    std::fs::create_dir_all(&cfg.work).expect("creating the work directory");
    let catalog = cfg.work.join("shared.sqlite");

    say(&format!("work dir: {}", cfg.work.display()));
    say(&format!(
        "cores: {}   levels: {:?}   {} rep(s) x {}s per scenario",
        available_cores(),
        cfg.levels,
        cfg.reps,
        cfg.seconds
    ));

    // --- Fixtures ----------------------------------------------------
    let t0 = Instant::now();
    build_catalog(&catalog, &cfg);
    say(&format!(
        "catalog: {} in {:.1}s",
        human_bytes(file_len(&catalog)),
        t0.elapsed().as_secs_f64()
    ));

    let tenants = provision_tenants(&cfg, &catalog, max_readers);
    say(&format!(
        "{} tenant databases, {} owned rows each",
        tenants.len(),
        cfg.owned_per_tenant
    ));

    // The control arm's private catalog copies. One per reader slot, made
    // once and reused across levels.
    let copies = catalog_copies(&cfg, &catalog, max_readers);
    say(&format!(
        "{} private catalog copies (control arm)",
        copies.len()
    ));

    let set_codes = set_codes(&cfg);

    // --- Scenarios ---------------------------------------------------
    //
    // The four arms at a given N run back to back, and the whole sweep
    // repeats. Both matter. Adjacency is what stops a slow patch of the
    // machine — another process, a thermal dip — from landing entirely on
    // one arm and reading as a difference between them; repetition is what
    // gets the sample count up, because one binder page at this catalog
    // scale costs ~100ms and a single pass yields only a hundred or so.
    // Latencies from every repetition are pooled before percentiles.
    let mut samples: BTreeMap<(&'static str, usize), Samples> = BTreeMap::new();

    for rep in 1..=cfg.reps {
        say(&format!("repetition {rep}/{}", cfg.reps));
        for &n in &cfg.levels {
            let shared_attach: Vec<PathBuf> = (0..n).map(|_| catalog.clone()).collect();
            let one_tenant: Vec<PathBuf> = vec![tenants[0].clone(); n];

            let arms = [
                ("shared", &tenants, &shared_attach, None),
                ("private", &tenants, &copies[..n].to_vec(), None),
                (
                    "same_tenant",
                    &one_tenant,
                    &shared_attach,
                    Some(Sharing::OneConnection),
                ),
                (
                    "refresh",
                    &tenants,
                    &shared_attach,
                    Some(Sharing::WithWriter),
                ),
            ];
            for (scenario, tenant_dbs, attach, sharing) in arms {
                let s = read_scenario(scenario, n, tenant_dbs, attach, &set_codes, &cfg, sharing);
                samples.entry((scenario, n)).or_default().absorb(s);
            }
        }
    }

    // The writer with no readers at all — the control for the WAL-growth
    // claim in the `refresh` arm. Without it, "the WAL grew" says nothing
    // about whether readers are what kept it from being checkpointed.
    let writer_only = writer_only_scenario(&catalog, &cfg);

    report(&mut samples, &writer_only, &cfg);
}

// ---------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------

/// Deterministic pseudo-random stream. Not cryptographic and not meant to
/// be: the point is that two runs of this harness generate byte-identical
/// catalogs, so a latency difference between runs is never a data difference.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn upto(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn set_codes(cfg: &Config) -> Vec<String> {
    (0..cfg.sets).map(|i| format!("s{i:04}")).collect()
}

/// Cards in set `i`. Real sets are not uniform — a 30-card promo set and a
/// 400-card special expansion cost very different amounts to page — so the
/// harness spreads around the configured mean rather than flattening it.
fn cards_in_set(i: usize, cfg: &Config) -> usize {
    let spread = cfg.cards_per_set / 2;
    cfg.cards_per_set - spread + (i * 37) % (2 * spread + 1)
}

/// Generate a catalog the size and shape of the real one. Skipped if the
/// file is already there, so re-running the harness against a built work dir
/// is fast.
fn build_catalog(path: &Path, cfg: &Config) {
    if path.exists() {
        say("catalog already built — reusing it");
        return;
    }
    let mut conn = pkdump_db::open_shared(path).expect("opening the catalog");

    // Variant codes come from the seeded `variants` table rather than a
    // literal list: `printings.variant` is a foreign key into it, and the
    // seed file is free to change.
    let variants: Vec<String> = conn
        .prepare("SELECT code FROM variants ORDER BY rank, code LIMIT 6")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert!(!variants.is_empty(), "the variants table is empty");

    let filler = "x".repeat(cfg.card_filler);
    let mut rng = Lcg(0x5eed_1234);
    let mut product_id: i64 = 100_000;

    let tx = conn.transaction().unwrap();
    {
        let mut set_stmt = tx
            .prepare(
                "INSERT INTO sets (set_code, name, series, total, printed_total, \
                 release_date, ptcgio_fetched_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .unwrap();
        let mut card_stmt = tx
            .prepare(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, \
                 supertype, rarity, artist, image_small, image_large, raw_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )
            .unwrap();
        let mut printing_stmt = tx
            .prepare(
                "INSERT INTO printings (printing_id, card_id, variant, language, \
                 tcgplayer_product_id, sub_type_name) VALUES (?1, ?2, ?3, 'en', ?4, ?5)",
            )
            .unwrap();
        let mut price_stmt = tx
            .prepare(
                "INSERT INTO latest_prices (tcgplayer_product_id, sub_type_name, source, \
                 price_type, price, observed_at) \
                 VALUES (?1, ?2, 'tcgcsv', 'market', ?3, '2026-08-01')",
            )
            .unwrap();

        for (i, set_code) in set_codes(cfg).iter().enumerate() {
            let n = cards_in_set(i, cfg);
            // printed_total below total, so the harness exercises the
            // secret/promo sectioning the real page does.
            let printed_total = (n * 9 / 10).max(1);
            set_stmt
                .execute(rusqlite::params![
                    set_code,
                    format!("Harness Set {i}"),
                    format!("Harness Series {}", i / 12),
                    n as i64,
                    printed_total as i64,
                    "2024-01-01",
                    "2026-08-01",
                ])
                .unwrap();

            for c in 1..=n {
                let card_id = format!("{set_code}-{c}");
                card_stmt
                    .execute(rusqlite::params![
                        card_id,
                        set_code,
                        c.to_string(),
                        c as i64,
                        format!("Card {c} of {set_code}"),
                        "Pokémon",
                        RARITIES[rng.upto(RARITIES.len())],
                        "Harness Artist",
                        format!("https://example.invalid/{card_id}/small.png"),
                        format!("https://example.invalid/{card_id}/large.png"),
                        filler,
                    ])
                    .unwrap();

                // 1–3 printings per card, which is roughly what variant
                // expansion produces across a modern set.
                for v in 0..(1 + rng.upto(3)) {
                    product_id += 1;
                    let variant = &variants[v % variants.len()];
                    let sub_type = if v == 0 { "Normal" } else { "Holofoil" };
                    printing_stmt
                        .execute(rusqlite::params![
                            format!("{card_id}-{variant}-en"),
                            card_id,
                            variant,
                            product_id,
                            sub_type,
                        ])
                        .unwrap();
                    price_stmt
                        .execute(rusqlite::params![
                            product_id,
                            sub_type,
                            (rng.upto(50_000) as f64) / 100.0,
                        ])
                        .unwrap();
                }
            }
        }
    }
    tx.commit().unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); ANALYZE;")
        .unwrap();
}

const RARITIES: &[&str] = &[
    "Common",
    "Uncommon",
    "Rare",
    "Rare Holo",
    "Double Rare",
    "Illustration Rare",
    "Special Illustration Rare",
    "Hyper Rare",
];

/// One collection database per reader slot, provisioned through the real
/// `pkdump tenant create` path so the layout under test is the shipped one.
fn provision_tenants(cfg: &Config, catalog: &Path, count: usize) -> Vec<PathBuf> {
    // `tenants::create` resolves its path from `$PKDUMP_HOME`. Pointing that
    // at the work dir is what keeps the harness off any real data directory.
    unsafe { std::env::set_var("PKDUMP_HOME", &cfg.work) };

    // Every printing in the catalog, to draw owned rows from.
    let printing_ids: Vec<String> = {
        let conn = Connection::open(catalog).unwrap();
        let mut stmt = conn
            .prepare("SELECT printing_id FROM printings ORDER BY printing_id")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };

    let mut paths = Vec::with_capacity(count);
    for i in 0..count {
        let name = format!("tenant{i:02}");
        let path = match pkdump_db::tenants::create(&name) {
            Ok(p) => {
                seed_collection(&p, &printing_ids, cfg.owned_per_tenant, i as u64);
                p
            }
            // Already provisioned by an earlier run against this work dir.
            Err(_) => pkdump_db::tenant_db_path(&name).unwrap(),
        };
        paths.push(path);
    }
    paths
}

/// Fill a tenant's `collection` with owned rows spread across the catalog, so
/// the binder page's owned-count subquery has real work to do on every slot.
fn seed_collection(tenant: &Path, printing_ids: &[String], owned: usize, seed: u64) {
    let mut conn = pkdump_db::open_user(tenant).unwrap();
    let mut rng = Lcg(0xc0ffee ^ seed);
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO collection (printing_id, condition, language, acquired_at, source) \
                 VALUES (?1, 'Near Mint', 'English', '2026-01-01', 'manual_id')",
            )
            .unwrap();
        for _ in 0..owned {
            stmt.execute([&printing_ids[rng.upto(printing_ids.len())]])
                .unwrap();
        }
    }
    tx.commit().unwrap();
}

/// Private catalog copies for the control arm — one per reader slot.
fn catalog_copies(cfg: &Config, catalog: &Path, count: usize) -> Vec<PathBuf> {
    let dir = cfg.work.join("catalog-copies");
    std::fs::create_dir_all(&dir).unwrap();
    (0..count)
        .map(|i| {
            let dest = dir.join(format!("shared{i:02}.sqlite"));
            if !dest.exists() {
                std::fs::copy(catalog, &dest).expect("copying the catalog");
            }
            dest
        })
        .collect()
}

// ---------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Sharing {
    /// Every worker contends for one tenant connection behind one mutex —
    /// the intra-tenant case the server has by construction.
    OneConnection,
    /// A writer commits to the shared catalog for the duration.
    WithWriter,
}

/// Raw measurements for one (scenario, reader-count) cell, pooled across
/// repetitions. Percentiles are taken from the pool at report time rather
/// than averaged per repetition — averaging percentiles is not a percentile.
#[derive(Default)]
struct Samples {
    /// Every completed operation's latency, in microseconds.
    latencies: Vec<u64>,
    /// Wall-clock seconds summed across repetitions, for ops/sec.
    elapsed: f64,
    /// `SQLITE_BUSY` / "database is locked" outcomes. The number this whole
    /// exercise exists to look at.
    busy: u64,
    /// `refresh` only.
    writer_commits: u64,
    /// `refresh` only: the largest WAL any repetition left behind.
    wal_bytes: u64,
}

impl Samples {
    fn absorb(&mut self, other: Samples) {
        self.latencies.extend(other.latencies);
        self.elapsed += other.elapsed;
        self.busy += other.busy;
        self.writer_commits += other.writer_commits;
        self.wal_bytes = self.wal_bytes.max(other.wal_bytes);
    }

    fn ops(&self) -> u64 {
        self.latencies.len() as u64
    }

    fn ops_per_sec(&self) -> f64 {
        self.latencies.len() as f64 / self.elapsed.max(f64::MIN_POSITIVE)
    }

    /// Percentile in microseconds. Assumes `latencies` is sorted.
    fn pct(&self, p: f64) -> u64 {
        if self.latencies.is_empty() {
            return 0;
        }
        let idx = ((self.latencies.len() - 1) as f64 * p).round() as usize;
        self.latencies[idx]
    }
}

fn read_scenario(
    scenario: &'static str,
    readers: usize,
    tenant_dbs: &[PathBuf],
    attach: &[PathBuf],
    set_codes: &[String],
    cfg: &Config,
    sharing: Option<Sharing>,
) -> Samples {
    // Each repetition of the writer arm starts from a checkpointed WAL, so
    // the size left behind is that repetition's growth and not a running
    // total across the sweep.
    if sharing == Some(Sharing::WithWriter) {
        truncate_wal(&attach[0]);
    }

    // Connections are opened before the clock starts: the server opens one
    // per tenant on that tenant's first request and keeps it, so open cost
    // is not part of the steady-state read path being measured.
    let one_connection = sharing == Some(Sharing::OneConnection);
    let conns: Vec<Arc<std::sync::Mutex<Connection>>> = if one_connection {
        let shared = Arc::new(std::sync::Mutex::new(
            pkdump_db::connect_user(&tenant_dbs[0], &attach[0]).expect("connecting"),
        ));
        (0..readers).map(|_| shared.clone()).collect()
    } else {
        (0..readers)
            .map(|i| {
                Arc::new(std::sync::Mutex::new(
                    pkdump_db::connect_user(&tenant_dbs[i], &attach[i]).expect("connecting"),
                ))
            })
            .collect()
    };

    // Read every file this scenario will touch into the page cache first.
    // The control arm holds N copies of the catalog and the shared arm holds
    // one, so without this the comparison would mostly measure how much of
    // each arm's working set the kernel happened to be caching — an IO
    // difference dressed up as a locking difference.
    let warm: std::collections::BTreeSet<&PathBuf> =
        tenant_dbs[..readers].iter().chain(attach.iter()).collect();
    for path in warm {
        prewarm(path);
    }

    let stop = Arc::new(AtomicBool::new(false));
    let busy = Arc::new(AtomicU64::new(0));
    // readers + the writer, when there is one, + this thread.
    let writer = sharing == Some(Sharing::WithWriter);
    let barrier = Arc::new(Barrier::new(readers + 1 + usize::from(writer)));

    let writer_commits = Arc::new(AtomicU64::new(0));
    let writer_handle = writer.then(|| {
        let catalog = attach[0].clone();
        let (stop, barrier, commits) = (stop.clone(), barrier.clone(), writer_commits.clone());
        std::thread::spawn(move || refresh_writer(&catalog, &stop, &barrier, &commits))
    });

    let handles: Vec<_> = (0..readers)
        .map(|i| {
            let conn = conns[i].clone();
            let (stop, busy, barrier) = (stop.clone(), busy.clone(), barrier.clone());
            // Each worker starts at a different set so the workers are not
            // all replaying one another's page-cache hits.
            let mut codes: Vec<String> = set_codes.to_vec();
            codes.rotate_left((i * set_codes.len() / readers.max(1)) % set_codes.len());
            std::thread::spawn(move || {
                let mut latencies: Vec<u64> = Vec::with_capacity(4096);
                let q = BinderQuery::default();
                barrier.wait();
                let mut idx = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    let code = &codes[idx % codes.len()];
                    idx += 1;
                    let started = Instant::now();
                    let guard = conn.lock().expect("connection mutex poisoned");
                    let outcome = get_binder_page(&guard, code, &q);
                    drop(guard);
                    match outcome {
                        Ok(Some(_)) => latencies.push(started.elapsed().as_micros() as u64),
                        Ok(None) => panic!("set {code} is missing from the catalog"),
                        Err(e) => {
                            // SQLITE_BUSY is the signal this whole exercise
                            // is looking for; count it rather than dying, so
                            // the run still produces numbers.
                            let text = e.to_string();
                            assert!(
                                text.contains("locked") || text.contains("busy"),
                                "reader failed for a reason that is not contention: {text}"
                            );
                            busy.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                latencies
            })
        })
        .collect();

    barrier.wait();
    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(cfg.seconds));
    stop.store(true, Ordering::Relaxed);
    let elapsed = started.elapsed().as_secs_f64();
    // Stat the WAL here, with every connection still open. SQLite checkpoints
    // and DELETES the -wal file when the last connection to a database
    // closes, so measuring after the joins would report 0 for any arm whose
    // connections happen to have gone away — an artifact that looks exactly
    // like "the checkpoint kept up".
    let wal_bytes = if writer {
        file_len(&wal_path(&attach[0]))
    } else {
        0
    };

    let latencies: Vec<u64> = handles
        .into_iter()
        .flat_map(|h| h.join().expect("reader thread panicked"))
        .collect();
    if let Some(h) = writer_handle {
        h.join().expect("writer thread panicked");
    }

    let out = Samples {
        latencies,
        elapsed,
        busy: busy.load(Ordering::Relaxed),
        writer_commits: writer_commits.load(Ordering::Relaxed),
        wal_bytes,
    };
    say(&format!(
        "  {:<12} n={:<3} {:>7} ops in {:.1}s  ({:.0} ops/s)",
        scenario,
        readers,
        out.ops(),
        elapsed,
        out.ops_per_sec()
    ));
    out
}

/// The writer running alone against the catalog, for the same duration and
/// with the same commit loop as the `refresh` arm. The comparison of the two
/// WAL sizes is what turns "the WAL grew" into "readers are why".
fn writer_only_scenario(catalog: &Path, cfg: &Config) -> (u64, u64) {
    truncate_wal(catalog);
    let stop = Arc::new(AtomicBool::new(false));
    let commits = Arc::new(AtomicU64::new(0));
    let barrier = Arc::new(Barrier::new(2));
    let handle = {
        let (catalog, stop, barrier, commits) = (
            catalog.to_path_buf(),
            stop.clone(),
            barrier.clone(),
            commits.clone(),
        );
        std::thread::spawn(move || refresh_writer(&catalog, &stop, &barrier, &commits))
    };
    barrier.wait();
    std::thread::sleep(Duration::from_secs(cfg.seconds));
    stop.store(true, Ordering::Relaxed);
    // Before the join, for the reason given in `read_scenario` — the writer's
    // connection is the only one open here, so joining first would delete the
    // WAL and report a spurious zero.
    let wal = file_len(&wal_path(catalog));
    handle.join().expect("writer thread panicked");
    say(&format!(
        "  {:<12} n=0   {:>7} commits, WAL {}",
        "writer_only",
        commits.load(Ordering::Relaxed),
        human_bytes(wal)
    ));
    (commits.load(Ordering::Relaxed), wal)
}

/// A database's write-ahead log. `Path::with_extension` would eat the
/// `.sqlite` and give `shared.sqlite-wal` only by accident of the name; this
/// is the same rule SQLite uses — append, do not replace.
fn wal_path(db: &Path) -> PathBuf {
    let mut p = db.as_os_str().to_owned();
    p.push("-wal");
    PathBuf::from(p)
}

/// Reset the WAL between scenarios so each one's growth starts from zero.
fn truncate_wal(db: &Path) {
    let conn = Connection::open(db).expect("opening the catalog to checkpoint it");
    conn.busy_timeout(Duration::from_secs(10)).unwrap();
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        .expect("checkpointing the catalog");
}

/// Model `pkdump data refresh` writing to the catalog while the server
/// serves: a real read-write connection committing small transactions until
/// told to stop.
fn refresh_writer(catalog: &Path, stop: &AtomicBool, barrier: &Barrier, commits: &AtomicU64) {
    let conn = Connection::open(catalog).expect("opening the catalog read-write");
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
        .unwrap();
    let rows: i64 = conn
        .query_row("SELECT max(rowid) FROM latest_prices", [], |r| r.get(0))
        .unwrap();
    barrier.wait();
    let mut seed = Lcg(0xfeed);
    while !stop.load(Ordering::Relaxed) {
        // A price refresh is exactly this: rewrite `latest_prices` rows.
        conn.execute(
            "UPDATE latest_prices SET price = price + 0.01, observed_at = '2026-08-07' \
             WHERE rowid BETWEEN ?1 AND ?1 + 500",
            [seed.upto(rows as usize) as i64],
        )
        .expect("the refresh writer could not commit");
        commits.fetch_add(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------

const SCENARIOS: [&str; 4] = ["shared", "private", "same_tenant", "refresh"];

fn report(
    samples: &mut BTreeMap<(&'static str, usize), Samples>,
    writer_only: &(u64, u64),
    cfg: &Config,
) {
    for s in samples.values_mut() {
        s.latencies.sort_unstable();
    }

    println!();
    println!(
        "N readers x {} sets, {} rep(s) x {}s, {} cores",
        cfg.sets,
        cfg.reps,
        cfg.seconds,
        available_cores()
    );
    println!();
    println!(
        "{:<12} {:>3} {:>8} {:>9} {:>10} {:>10} {:>10} {:>10} {:>6}",
        "scenario", "n", "ops", "ops/s", "p50 ms", "p95 ms", "p99 ms", "max ms", "busy"
    );
    for scenario in SCENARIOS {
        for &n in &cfg.levels {
            let Some(s) = samples.get(&(scenario, n)) else {
                continue;
            };
            println!(
                "{:<12} {:>3} {:>8} {:>9.1} {:>10.1} {:>10.1} {:>10.1} {:>10.1} {:>6}",
                scenario,
                n,
                s.ops(),
                s.ops_per_sec(),
                ms(s.pct(0.50)),
                ms(s.pct(0.95)),
                ms(s.pct(0.99)),
                ms(s.latencies.last().copied().unwrap_or(0)),
                s.busy,
            );
        }
    }

    // The headline. `shared` is the epic's design; `private` is the same
    // load with the catalog NOT shared. A ratio at 1.00 means one `ATTACH`ed
    // catalog costs the same as N — that sharing is free — and separates the
    // cost of sharing from the cost of running N readers on this many cores.
    println!();
    println!("shared ÷ private at the same reader count (1.00 = sharing costs nothing)");
    println!(
        "{:>3} {:>12} {:>12} {:>14}",
        "n", "p50 ratio", "p95 ratio", "ops/s ratio"
    );
    for &n in &cfg.levels {
        let (Some(s), Some(p)) = (samples.get(&("shared", n)), samples.get(&("private", n))) else {
            continue;
        };
        println!(
            "{:>3} {:>12.2} {:>12.2} {:>14.2}",
            n,
            ratio(s.pct(0.50), p.pct(0.50)),
            ratio(s.pct(0.95), p.pct(0.95)),
            s.ops_per_sec() / p.ops_per_sec().max(f64::MIN_POSITIVE),
        );
    }

    // What the WAL does under a writer, with and without readers present.
    println!();
    println!(
        "catalog WAL left behind by the writer, per {}s run",
        cfg.seconds
    );
    println!("{:>18} {:>12} {:>14}", "readers", "commits", "WAL");
    println!(
        "{:>18} {:>12} {:>14}",
        "0 (control)",
        writer_only.0,
        human_bytes(writer_only.1)
    );
    for &n in &cfg.levels {
        let Some(s) = samples.get(&("refresh", n)) else {
            continue;
        };
        println!(
            "{:>18} {:>12} {:>14}",
            n,
            s.writer_commits / cfg.reps.max(1) as u64,
            human_bytes(s.wal_bytes)
        );
    }

    let busy: u64 = samples.values().map(|s| s.busy).sum();
    println!();
    println!("SQLITE_BUSY / \"database is locked\" across every scenario: {busy}");

    // Machine-readable, for the write-up and for any future comparison.
    let json = serde_json::json!({
        "cores": available_cores(),
        "seconds_per_scenario": cfg.seconds,
        "reps": cfg.reps,
        "sets": cfg.sets,
        "levels": cfg.levels,
        "writer_only": { "commits": writer_only.0, "wal_bytes": writer_only.1 },
        "rows": samples.iter().map(|((scenario, n), s)| serde_json::json!({
            "scenario": scenario,
            "readers": n,
            "ops": s.ops(),
            "ops_per_sec": s.ops_per_sec(),
            "p50_us": s.pct(0.50),
            "p95_us": s.pct(0.95),
            "p99_us": s.pct(0.99),
            "max_us": s.latencies.last().copied().unwrap_or(0),
            "busy": s.busy,
            "writer_commits": s.writer_commits,
            "wal_bytes": s.wal_bytes,
        })).collect::<Vec<_>>(),
    });
    let out = cfg.work.join("result.json");
    std::fs::write(&out, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    println!("machine-readable results: {}", out.display());
}

fn ratio(a: u64, b: u64) -> f64 {
    a as f64 / (b.max(1) as f64)
}

fn ms(micros: u64) -> f64 {
    micros as f64 / 1000.0
}

/// Pull a database file through the page cache so a scenario starts warm.
fn prewarm(path: &Path) {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else {
        return;
    };
    let mut buf = vec![0u8; 1 << 20];
    while matches!(f.read(&mut buf), Ok(n) if n > 0) {}
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn human_bytes(n: u64) -> String {
    match n {
        0 => "0".to_string(),
        n if n >= 1 << 30 => format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1u64 << 20) as f64),
        n => format!("{:.1} KiB", n as f64 / 1024.0),
    }
}

fn available_cores() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0)
}

fn say(msg: &str) {
    println!("[attach-concurrency] {msg}");
}
