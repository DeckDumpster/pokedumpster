//! The catalog's WAL, and what gives it back (pd-t50h).
//!
//! # The bug these tests are about
//!
//! In WAL mode a checkpoint can copy frames into the database while readers
//! are active, but it cannot *reset* the WAL — restart it at frame 0 — until a
//! moment arrives with nobody reading. So a writer with any reader in flight
//! appends for its whole write window, and the `-wal` file then keeps its
//! high-water mark until something truncates it. Measured on the catalog in
//! `deep-dives/attach-concurrency` (RESULT.md finding B): the same writer for
//! the same ten seconds left **4.0 MiB** with no readers and **914 MiB** with
//! one. A ~230x difference, binary rather than proportional.
//!
//! Nothing on the box repaired it. An autocheckpoint runs on a commit; the
//! nightly derive is the only thing that commits to the catalog, and it exits
//! when it is done — so the file it left sat on the data volume until the next
//! night's run happened to reset the WAL.
//!
//! # What is claimed here, and what is not
//!
//! Claimed: an opportunistic truncating checkpoint driven from inside the write
//! loop bounds the file, and one at the end of the window returns it, both
//! while a reader is still going.
//!
//! **Not** claimed: that the WAL is bounded against a reader holding ONE read
//! transaction open across the whole window. Nothing can reset a WAL under
//! that, and `a_reader_holding_one_transaction_blocks_the_reset` states it as a
//! test rather than leaving it to be discovered. It is not the shape this
//! deployment has — `pkdump-server` opens a connection per request and every
//! query is short — and the mitigation for it is a different, larger change
//! (derive to a copy and swap), filed rather than smuggled in here.
//!
//! # The mitigation that was measured and REJECTED
//!
//! `PRAGMA journal_size_limit`, the first of the three the bead lists. It
//! truncates the WAL when a checkpoint resets it — which is exactly the thing
//! that never happens here. Measured against the mechanism, 1 looping reader,
//! growth per commit:
//!
//! | window | no limit | journal_size_limit = 16 MiB |
//! |---|---|---|
//! |  3 s | 2.46 KiB/commit | 3.72 KiB/commit |
//! |  6 s | 4.02 KiB/commit | 4.02 KiB/commit |
//! | 12 s | 2.30 KiB/commit | 4.02 KiB/commit |
//!
//! Identical inside the noise, and the 12 s arm reached 289 MiB *with* the
//! limit set. It is not in the code, deliberately: a setting that does nothing
//! for the workload it was added for is worse than no setting, because the next
//! person reads it as the fix and stops looking.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::Connection;

/// A WAL-mode database with a few thousand rows in it, checkpointed clean, so
/// every byte a test then measures came from that test's own writing.
fn seeded(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; \
         PRAGMA synchronous = NORMAL; \
         CREATE TABLE t (id INTEGER PRIMARY KEY, blob BLOB);",
    )
    .unwrap();
    conn.busy_timeout(pkdump_db::BUSY_TIMEOUT).unwrap();
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut stmt = tx.prepare("INSERT INTO t (blob) VALUES (?1)").unwrap();
        for i in 0..ROWS {
            stmt.execute(rusqlite::params![payload(i)]).unwrap();
        }
    }
    tx.commit().unwrap();
    pkdump_db::checkpoint_truncate(&conn, Duration::from_secs(10)).unwrap();
    conn
}

/// Rows in the fixture. Small enough to stay quick, large enough that a run of
/// [`COMMITS`] updates spreads over many pages rather than rewriting one.
const ROWS: i64 = 4_000;

/// A blob that is different for every `n` there will ever be.
///
/// Not decoration. SQLite **skips a row overwrite whose bytes are identical**
/// (`btreeOverwriteContent`), so an update loop that happens to rewrite the
/// value already in the row journals nothing at all — the WAL stays at zero
/// bytes and every measurement below reads as a perfect result. The first
/// version of this file did exactly that.
fn payload(n: i64) -> Vec<u8> {
    let mut v = vec![0u8; 200];
    v[..8].copy_from_slice(&n.to_le_bytes());
    v
}

/// Keeps an update's bytes clear of the seed's, for the same reason. Row `r`
/// is born holding `payload(r)`, and the first pass of the write loop would
/// otherwise hand it exactly that back.
const UPDATE_SALT: i64 = 1_000_000;

fn wal_bytes(path: &Path) -> u64 {
    let mut p = path.as_os_str().to_owned();
    p.push("-wal");
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

/// A reader that pins a snapshot most of the time and lets go briefly — the
/// shape a checkpoint has to slip through.
///
/// Duty-cycled with **sleeps** rather than by querying flat out, and that is
/// what makes the tests below say the same thing on a loaded box as on an idle
/// one. A reader spinning on queries holds its snapshot for a fraction of the
/// time that depends entirely on how much CPU it gets; these sleeps dominate,
/// so the fraction is a property of the harness. CI runs three container gates
/// beside this one on a four-core box.
/// A reader that holds ONE snapshot for the whole run, used only by the SEEN-RED arm.
///
/// [`DutyCycledReader`] models how the server actually reads — a snapshot held ~20ms,
/// released, taken again — and that is the right instrument for testing the FIX. It is the
/// wrong one for proving the hazard exists, because its duty cycle depends on getting the
/// CPU back promptly after each gap. Under a loaded box the gaps stretch while the held
/// windows do not, the snapshot is absent for most of the run, SQLite checkpoints freely,
/// and the WAL stays small.
///
/// That is measurement, not behaviour: the same code measured 12x on an idle machine and
/// 2.3x inside CI's parallel suite, below a 5x floor, so the arm asserting the bug
/// REPRODUCES reported that it could not. One snapshot held for the duration cannot be
/// descheduled out of existence -- the WAL cannot be checkpointed past it whatever the
/// scheduler does.
struct HeldReader {
    stop: Arc<AtomicBool>,
    reads: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl HeldReader {
    fn start(path: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicU64::new(0));
        let p = path.to_path_buf();
        let (s, r) = (Arc::clone(&stop), Arc::clone(&reads));
        let handle = std::thread::spawn(move || {
            let conn = Connection::open(&p).unwrap();
            conn.busy_timeout(Duration::from_secs(30)).unwrap();
            conn.execute_batch("BEGIN").unwrap();
            let _: i64 = conn
                .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
                .unwrap();
            r.fetch_add(1, Ordering::Relaxed);
            while !s.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(2));
            }
            conn.execute_batch("COMMIT").unwrap();
        });
        while reads.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        Self {
            stop,
            reads,
            handle: Some(handle),
        }
    }

    fn stop(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
        self.reads.load(Ordering::Relaxed)
    }
}

struct DutyCycledReader {
    stop: Arc<AtomicBool>,
    reads: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// How long a snapshot is pinned, and the gap left between two of them.
const READ_HELD: Duration = Duration::from_millis(20);
const READ_GAP: Duration = Duration::from_millis(4);

/// The reclaim period the bounding test drives, a couple of the reader's
/// cycles. Production's is [`WalReclaim::new`]'s five seconds against a write
/// window of minutes; this is the same ratio, scaled to a test.
const RECLAIM_PERIOD: Duration = Duration::from_millis(50);

impl DutyCycledReader {
    fn start(path: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let reads = Arc::new(AtomicU64::new(0));
        let p = path.to_path_buf();
        let (s, r) = (Arc::clone(&stop), Arc::clone(&reads));
        let handle = std::thread::spawn(move || {
            let conn = Connection::open(&p).unwrap();
            conn.busy_timeout(Duration::from_secs(30)).unwrap();
            while !s.load(Ordering::Relaxed) {
                conn.execute_batch("BEGIN").unwrap();
                let _: i64 = conn
                    .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
                    .unwrap();
                std::thread::sleep(READ_HELD);
                conn.execute_batch("COMMIT").unwrap();
                r.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(READ_GAP);
            }
        });
        // Do not start measuring until the reader is genuinely reading: a race
        // here would silently turn the whole file into the zero-reader control.
        while reads.load(Ordering::Relaxed) == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        Self {
            stop,
            reads,
            handle: Some(handle),
        }
    }

    fn stop(mut self) -> u64 {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.take().unwrap().join().unwrap();
        self.reads.load(Ordering::Relaxed)
    }
}

/// Commit exactly `commits` single-row updates, optionally reclaiming as it
/// goes, and report the WAL's high-water mark and its size at the end.
///
/// A **count**, never a duration: the arms below are compared per commit, and
/// a time-boxed writer gets a different number of them in depending on what
/// else the box is doing. The bead's own table falls at high reader counts for
/// exactly that reason.
fn commit_n(conn: &Connection, path: &Path, commits: u64, reclaim: Option<Duration>) -> (u64, u64) {
    let mut r = reclaim.map(pkdump_db::WalReclaim::every);
    let mut peak = 0u64;
    for n in 0..commits {
        conn.execute(
            "UPDATE t SET blob = ?1 WHERE id = ?2",
            rusqlite::params![payload(n as i64 + UPDATE_SALT), (n as i64 % ROWS) + 1],
        )
        .unwrap();
        peak = peak.max(wal_bytes(path));
        if let Some(r) = r.as_mut() {
            r.maybe(conn).unwrap();
        }
    }
    (peak, wal_bytes(path))
}

/// How many commits every comparison below is stated over.
///
/// A count is what makes these tests say the same thing on a loaded box as on
/// an idle one, and it is why the numbers come out repeatable: the control arm
/// is pinned at SQLite's 1000-page autocheckpoint bound (3.93 MiB, to the byte,
/// run after run) and the reader arm grows with the commits rather than with
/// the clock. Twelve thousand puts about an order of magnitude between them.
const COMMITS: u64 = 12_000;

/// The mechanism itself, stated so the tests after it have something to be
/// measured against. This is not a claim about our code — it is a claim about
/// SQLite, and the bead's whole finding is that the two arms differ by orders
/// of magnitude. A suite that only asserted the fix would go green against a
/// harness in which the bug never reproduced.
#[test]
fn a_reader_in_flight_unbounds_a_wal_that_would_otherwise_stay_small() {
    let dir = tempfile::tempdir().unwrap();

    let control_path = dir.path().join("control.sqlite");
    let control = seeded(&control_path);
    let (_, control_wal) = commit_n(&control, &control_path, COMMITS, None);

    let reader_path = dir.path().join("reader.sqlite");
    let writer = seeded(&reader_path);
    // Held, not duty-cycled: see HeldReader. A duty cycle makes this arm a
    // measurement of the scheduler.
    let reader = HeldReader::start(&reader_path);
    let (_, reader_wal) = commit_n(&writer, &reader_path, COMMITS, None);
    let reads = reader.stop();

    assert!(
        reads > 0,
        "the reader never ran — the arms are the same arm"
    );
    // Measured on this workload: 3.93 MiB with no reader (the autocheckpoint bound,
    // identical to the byte across runs) against ~47 MiB with one, over the same 12,000
    // commits. Five is a floor with an order of magnitude of room.
    //
    // That room is only real because the snapshot is HELD. With a duty-cycled reader the
    // same assertion measured 2.3x inside CI's parallel suite -- the reader starved, not
    // the bug absent.
    assert!(
        reader_wal > control_wal * 5,
        "the bug did not reproduce: {control_wal} bytes of WAL with no readers vs \
         {reader_wal} with one, over {COMMITS} commits each. Everything below is \
         measured against this."
    );
}

/// The fix, over the same commits the test above shows unbounded.
#[test]
fn an_opportunistic_reclaim_bounds_a_wal_a_reader_would_otherwise_unbound() {
    let dir = tempfile::tempdir().unwrap();

    let bare_path = dir.path().join("bare.sqlite");
    let bare = seeded(&bare_path);
    let bare_reader = DutyCycledReader::start(&bare_path);
    let (bare_peak, _) = commit_n(&bare, &bare_path, COMMITS, None);
    bare_reader.stop();

    let kept_path = dir.path().join("kept.sqlite");
    let kept = seeded(&kept_path);
    let kept_reader = DutyCycledReader::start(&kept_path);
    // A couple of the reader's own cycles: the point of the test is that a
    // checkpoint lands in a gap, and one that fired once over the whole run
    // would be testing the final reclaim instead. Production's period is five
    // seconds against a write window of minutes — the same ratio.
    let (kept_peak, _) = commit_n(&kept, &kept_path, COMMITS, Some(RECLAIM_PERIOD));
    let reads = kept_reader.stop();

    assert!(
        reads > 0,
        "the reader never ran — nothing was being contended"
    );
    assert!(
        kept_peak * 2 < bare_peak,
        "the reclaim did not bound the WAL: peak {bare_peak} bytes without it vs \
         {kept_peak} with it, over {COMMITS} commits each"
    );
}

/// The end of the write window — the half that gets the disk back rather than
/// merely bounding it. Asserted **while the reader is still reading**, because
/// that is the state the nightly derive finishes in, and a checkpoint measured
/// after the readers went away would be measuring nothing.
#[test]
fn a_truncating_checkpoint_returns_the_wal_with_a_reader_still_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    let conn = seeded(&path);
    let reader = DutyCycledReader::start(&path);

    let (_, wal_at_end) = commit_n(&conn, &path, COMMITS, None);
    assert!(
        wal_at_end > 1 << 20,
        "nothing accumulated to reclaim ({wal_at_end} bytes) — this test proves nothing"
    );

    let out = pkdump_db::checkpoint_truncate(&conn, Duration::from_secs(30)).unwrap();
    let after = wal_bytes(&path);
    let reads = reader.stop();

    assert!(reads > 0, "the reader never ran");
    assert!(out.reset, "the checkpoint did not reset the WAL: {out:?}");
    assert_eq!(
        after, 0,
        "reset was reported but the file is still {after} bytes"
    );
}

/// The honest limit, stated as a test so it cannot quietly stop being true.
///
/// A reader holding ONE transaction across the whole window pins a snapshot, so
/// no checkpoint can reset — and the right behaviour is to say so and carry on,
/// not to raise. A run that failed the nightly catalog build because somebody
/// was browsing would be a worse bug than the one being fixed.
#[test]
fn a_reader_holding_one_transaction_blocks_the_reset_and_that_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    let conn = seeded(&path);

    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN").unwrap();
    let _: i64 = holder
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();

    let (_, wal_at_end) = commit_n(&conn, &path, 500, None);
    assert!(wal_at_end > 0, "nothing was written");

    let out = pkdump_db::checkpoint_truncate(&conn, Duration::from_millis(200)).unwrap();
    assert!(
        !out.reset,
        "a held read transaction should block the reset, but the checkpoint claims it \
         succeeded: {out:?}. If SQLite has changed, the derive's warning is now a lie."
    );
    assert!(
        wal_bytes(&path) > 0,
        "the file was truncated under a held reader"
    );

    holder.execute_batch("COMMIT").unwrap();

    // And once the reader lets go, the same call reclaims it. Without this the
    // assertion above would pass against a `checkpoint_truncate` that never
    // works at all.
    let out = pkdump_db::checkpoint_truncate(&conn, Duration::from_secs(30)).unwrap();
    assert!(
        out.reset,
        "the reader is gone and it still did not reset: {out:?}"
    );
    assert_eq!(wal_bytes(&path), 0);
}

/// `WalReclaim` is periodic, not per-call. Getting this wrong would arrive
/// looking like "the derive got slower" rather than like a bug here, so both
/// ends are pinned: the production period does not fire inside a window
/// shorter than itself, and a zero period fires every time it is asked.
#[test]
fn a_reclaim_honours_its_period() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    let conn = seeded(&path);

    // The real constructor, over a window far shorter than the five seconds it
    // is built with. Stated as "not yet" rather than as a count, so a loaded
    // box cannot turn it into a different assertion.
    let mut prod = pkdump_db::WalReclaim::new();
    let start = Instant::now();
    let mut fired = 0;
    while start.elapsed() < Duration::from_millis(500) {
        if prod.maybe(&conn).unwrap().is_some() {
            fired += 1;
        }
    }
    assert_eq!(
        fired, 0,
        "the default period is meant to be seconds; it fired {fired} times in half of one"
    );

    let mut eager = pkdump_db::WalReclaim::every(Duration::ZERO);
    for _ in 0..3 {
        assert!(
            eager.maybe(&conn).unwrap().is_some(),
            "a zero period must fire every time, or the period is not what gates it"
        );
    }
}
