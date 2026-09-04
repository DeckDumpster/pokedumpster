//! The shared catalog has writers of two kinds, and a server start is not the
//! kind that builds one (pd-dzu5).
//!
//! `shared.sqlite` is built by `pkdump setup` and, nightly, by
//! `pkdump-lake-derive shared` — a job that holds the catalog's write lock,
//! in transactions of its own, for minutes. `pkdump serve` only ever
//! *converges* it: schema, any column the file predates, and the shipped
//! seeds. That was unconditional on every start, so a deploy or a reboot
//! landing inside the 07:00 derive lost the race, failed on `database is
//! locked` after five seconds, and `Restart=on-failure` retried it every
//! fifteen until the build was over — a silent outage for the rest of the
//! derive, because `deploy/pkdump.container` carries no `OnFailure=`.
//!
//! Every test here is stated against a catalog whose write lock is genuinely
//! held by another connection, and each has a control beside it: an assertion
//! that the same call *does* fail when there really is something to converge.
//! Without those, a `open_shared_for_serving` that had quietly stopped
//! converging anything at all would pass the whole file.

use std::time::Duration;

use rusqlite::Connection;

/// A converged catalog and a second connection sitting in a write
/// transaction on it — the derive, in miniature.
///
/// The holder is returned rather than dropped: SQLite releases the write lock
/// when the transaction ends, so it has to outlive the assertion.
fn catalog_with_the_write_lock_held(dir: &std::path::Path) -> (std::path::PathBuf, Connection) {
    let path = dir.join("shared.sqlite");
    // Converge it once, exactly as `pkdump setup` would.
    drop(pkdump_db::open_shared(&path).expect("building the catalog"));

    let holder = Connection::open(&path).expect("the second writer");
    holder
        .busy_timeout(Duration::from_secs(5))
        .expect("the second writer's patience");
    // BEGIN IMMEDIATE is the derive's shape: it takes the write lock at once
    // and leaves readers alone, which is why the failure this file is about
    // is a failed *start* rather than a served page going wrong.
    holder
        .execute_batch("BEGIN IMMEDIATE")
        .expect("taking the write lock");
    (path, holder)
}

/// The control. A read-write open of a catalog somebody else is writing fails
/// — which is what `pkdump serve` used to do on every single start.
#[test]
fn a_read_write_open_loses_to_a_writer_that_is_already_there() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _holder) = catalog_with_the_write_lock_held(dir.path());

    // A short patience so the test costs 200ms rather than 5s. The default is
    // 5s and the outcome is the same one; only the wait differs.
    let err = pkdump_db::open_shared_with_patience(&path, Duration::from_millis(200))
        .expect_err("a second writer must not be able to converge this catalog");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("locked") || msg.contains("busy"),
        "expected a lock failure, got: {err}"
    );
}

/// The fix. A start that has nothing to converge does not compete for the
/// lock at all, so the same catalog opens while the derive is mid-transaction.
#[test]
fn a_serving_open_of_a_converged_catalog_does_not_wait_for_the_writer() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _holder) = catalog_with_the_write_lock_held(dir.path());

    let started = std::time::Instant::now();
    let conn = pkdump_db::open_shared_for_serving(&path)
        .expect("a converged catalog needs nothing from a server start");
    // Not merely "it succeeded eventually": it must not have waited. Anything
    // near the 5s default means it took the write path and got lucky.
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the serving open waited {:?} — it took the write path",
        started.elapsed()
    );

    // And it is usable for what the server actually does with it.
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM search_keywords", [], |r| r.get(0))
        .expect("reading the search registry off the catalog");
    assert!(n > 0, "the search keyword registry must be seeded");
}

/// The other half of the same claim, and the one that makes the test above
/// mean something: the handle it gets back cannot write the catalog at all.
///
/// This is what "took no write lock" is asserted as, rather than as a
/// timestamp that did not move. `catalog_convergence` holds the fingerprint
/// and nothing else — a `converged_at` would make two derives of one raw/
/// partition differ, which is the property `pkdump-lake-derive diff` exists to
/// check — so the observable is the connection itself.
#[test]
fn a_serving_open_of_a_converged_catalog_hands_back_a_handle_that_cannot_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    drop(pkdump_db::open_shared(&path).unwrap());

    let conn = pkdump_db::open_shared_for_serving(&path).unwrap();
    let err = conn
        .execute(
            "INSERT INTO sets (set_code, name, series) VALUES ('zz', 'nope', 'nope')",
            [],
        )
        .expect_err("a serving open must not be able to write the catalog");
    assert!(
        err.to_string().to_lowercase().contains("readonly"),
        "expected a read-only connection, got: {err}"
    );
}

/// The inverse, and the reason the two tests above are not vacuous. A catalog
/// that is NOT converged still has to be, so the same call takes the write
/// path — and therefore waits for the writer that is already there rather
/// than sailing past it.
///
/// This is the case a binary upgrade shipping a data-only migration is in. It
/// is the only case left that can collide with the derive, which is the whole
/// of what pd-dzu5 changes. It also states the second half of the fix: the
/// wait is what turns that collision into a start that is a minute late
/// instead of one that fails after five seconds and is retried for the rest
/// of the build.
#[test]
fn a_serving_open_of_an_unconverged_catalog_waits_for_the_writer_and_then_converges() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    drop(pkdump_db::open_shared(&path).unwrap());
    // Un-converged the way a new build leaves it — and COMMITTED, because an
    // uncommitted change is not a fact about the catalog. (Asserting this
    // against an open transaction is how the first draft of this test fooled
    // itself: the read-only probe correctly saw the old, matching row.)
    {
        let c = Connection::open(&path).unwrap();
        c.execute(
            "UPDATE catalog_convergence SET fingerprint = 'not this build' WHERE id = 1",
            [],
        )
        .unwrap();
    }

    let holder = Connection::open(&path).unwrap();
    holder.busy_timeout(Duration::from_secs(5)).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let for_thread = path.clone();
    let worker = std::thread::spawn(move || {
        let r = pkdump_db::open_shared_for_serving(&for_thread);
        tx.send(()).ok();
        r.map(|_| ())
    });

    // Still waiting a second in: it took the write path, and it did not give
    // up at the five seconds an ordinary open would have used either.
    assert!(
        rx.recv_timeout(Duration::from_secs(1)).is_err(),
        "the serving open returned while the catalog was held — it skipped a convergence it owed"
    );

    holder.execute_batch("ROLLBACK").unwrap();
    drop(holder);
    worker
        .join()
        .unwrap()
        .expect("once the writer is gone the convergence must succeed");

    let c = Connection::open(&path).unwrap();
    let fingerprint: String = c
        .query_row(
            "SELECT fingerprint FROM catalog_convergence WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_ne!(
        fingerprint, "not this build",
        "the convergence must have re-recorded the fingerprint"
    );
    assert_eq!(fingerprint.len(), 64);
}

/// A catalog from a build older than the fingerprint table converges rather
/// than being taken on trust. Every existing box is in exactly this state on
/// the deploy that lands this change.
#[test]
fn a_catalog_with_no_fingerprint_table_is_converged_not_trusted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    drop(pkdump_db::open_shared(&path).unwrap());
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("DROP TABLE catalog_convergence").unwrap();
    }

    // No writer in the way, so this must succeed — and must have rebuilt the
    // row, which is the observable proof it took the write path.
    let conn = pkdump_db::open_shared_for_serving(&path).unwrap();
    let fingerprint: String = conn
        .query_row(
            "SELECT fingerprint FROM catalog_convergence WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .expect("the convergence must have been recorded");
    assert_eq!(fingerprint.len(), 64);
}

/// The fingerprint is recorded by opening the catalog read-write and by
/// nothing else, so a second open is a no-op and a third is too. Stated
/// because "converged" is a claim about work already done: if the value
/// moved on every open, nothing could ever be skipped.
#[test]
fn converging_twice_records_the_same_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    let read = |c: &Connection| -> String {
        c.query_row(
            "SELECT fingerprint FROM catalog_convergence WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let first = { read(&pkdump_db::open_shared(&path).unwrap()) };
    let second = { read(&pkdump_db::open_shared(&path).unwrap()) };
    assert_eq!(first, second);
}

/// The read-only probe discards every failure it meets, so the one failure
/// that must not be discarded gets its own assertion: a catalog written by a
/// NEWER build is refused, not served.
///
/// The refusal comes from the read-write open the probe falls through to,
/// which gates the same file again. Stated as a test because "it is refused
/// one call later" is exactly the kind of claim that is true when written and
/// quietly false after a refactor — and the failure would be a server happily
/// serving a schema it does not understand.
#[test]
fn a_catalog_from_a_newer_build_is_still_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    drop(pkdump_db::open_shared(&path).unwrap());
    {
        let c = Connection::open(&path).unwrap();
        let v: i64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        c.execute_batch(&format!("PRAGMA user_version = {}", v + 1))
            .unwrap();
    }

    let err = pkdump_db::open_shared_for_serving(&path)
        .expect_err("a catalog from the future must not be served");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("newer") || msg.contains("version"),
        "the refusal must name the version skew, got: {err}"
    );
}

/// A catalog carrying this build's fingerprint but not its version stamp is
/// NOT converged. `converge` persists two things and both have to be true.
///
/// This is the state every catalog in existence was in before pd-ja38 —
/// `user_version` 0 — and it is what `tests/schema-version/run.sh` §1 boots
/// and requires to come out stamped. A fingerprint-only check looks obviously
/// sufficient and is not: it reports converged a catalog this build has never
/// stamped, so the stamp never arrives and the file goes on claiming a shape
/// it does not have.
#[test]
fn a_fingerprint_without_the_version_stamp_is_not_a_converged_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shared.sqlite");
    drop(pkdump_db::open_shared(&path).unwrap());
    {
        // The fingerprint stays; only the stamp is rolled back.
        let c = Connection::open(&path).unwrap();
        c.execute_batch("PRAGMA user_version = 0").unwrap();
    }

    drop(pkdump_db::open_shared_for_serving(&path).unwrap());

    let c = Connection::open(&path).unwrap();
    let v: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert!(
        v > 0,
        "an unstamped catalog must be converged and stamped, not taken on the fingerprint alone"
    );
}
