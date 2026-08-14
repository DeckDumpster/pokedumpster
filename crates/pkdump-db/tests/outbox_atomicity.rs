//! The atomicity claim behind the ownership outbox (pd-5m54), proven the
//! only way it can be: by killing a writer mid-flight and looking at what
//! survived.
//!
//! The claim is that a holding and the event describing it are never
//! observed to disagree. A test that mutates a collection and then finds the
//! event does not prove that — it proves the happy path, which was never in
//! doubt. What the inbound-leg design actually needs is that a crash cannot
//! leave the two apart, because the offline side's consistency is built on
//! it: an event lost in a crash is a holding the lakehouse never learns
//! about, and nothing downstream can tell that it is missing.
//!
//! So: a child process writes batches of mutations in a loop, the parent
//! SIGKILLs it at a delay that lands inside one of them, and the parent then
//! replays the outbox from seq 1 and compares the result to the collection
//! table row for row. Every row. Every iteration.
//!
//! SIGKILL rather than a signal handler or a panic on purpose — it cannot be
//! caught, deferred or cleaned up after, so nothing in this process gets a
//! chance to tidy the two into agreement.
//!
//! The child is this same test binary, re-executed with
//! `PKDUMP_OUTBOX_CRASH_DB` set; `the_child_that_gets_killed` is its entry
//! point and is `#[ignore]`d so a normal run never starts one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Map, Value};

/// Set on the child; holds the collection database it should write to.
const DB_ENV: &str = "PKDUMP_OUTBOX_CRASH_DB";

/// How many kills to run. Each is an independent database.
const ITERATIONS: usize = 16;

/// Mutations per transaction in the child. Large enough that a batch takes
/// long enough to be killed in the middle of.
const BATCH: usize = 400;

// ---------------------------------------------------------------------
// The parent
// ---------------------------------------------------------------------

#[test]
fn a_killed_writer_never_leaves_the_collection_and_the_outbox_disagreeing() {
    let exe = std::env::current_exe().expect("the test binary re-executes itself as the child");

    let mut torn = 0usize;
    let mut with_rows = 0usize;

    for iteration in 0..ITERATIONS {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("collection.sqlite");
        let marker = dir.path().join("in-flight");
        // Create the database (and its triggers) before the child starts, so
        // the child's first act is a mutation rather than a schema apply.
        drop(pkdump_db::open_user(&db).unwrap());

        let mut child = Command::new(&exe)
            .args([
                "--exact",
                "the_child_that_gets_killed",
                "--ignored",
                "--test-threads",
                "1",
            ])
            .env(DB_ENV, &db)
            .env("PKDUMP_OUTBOX_CRASH_MARKER", &marker)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the child writer");

        std::thread::sleep(Duration::from_millis(kill_delay_ms(iteration)));
        // SIGKILL. Uncatchable — the child gets no chance to finish a
        // transaction, close a connection or flush anything.
        child.kill().expect("kill the child writer");
        child.wait().expect("reap the child writer");

        // --- the assertion -------------------------------------------
        let conn = pkdump_db::open_user(&db).unwrap();
        let live = live_collection(&conn);
        let projected = replay_outbox(&conn);

        if let Some(disagreement) = first_disagreement(&projected, &live) {
            panic!(
                "iteration {iteration}: the outbox replayed to a different \
                 collection than the one on disk\n{disagreement}"
            );
        }

        // --- and the evidence that it was worth asserting -------------
        // `in-flight` holds the two counts the batch being written would have
        // reached. It survives the kill because the child never gets to
        // remove it. Two counts rather than one because an update batch moves
        // no rows in or out — only the sold tally moves, and a kill inside one
        // would otherwise look like a batch that never started.
        if let Ok(target) = std::fs::read_to_string(&marker) {
            let (rows, sold) = target.trim().split_once(':').expect("<rows>:<sold>");
            let rows: usize = rows.parse().unwrap();
            let sold: usize = sold.parse().unwrap();
            if live.len() != rows || live_sold(&live) != sold {
                torn += 1;
            }
        }
        if !live.is_empty() {
            with_rows += 1;
        }
    }

    assert!(
        with_rows >= ITERATIONS / 2,
        "only {with_rows}/{ITERATIONS} kills left any rows behind — the child \
         is being killed before it writes, so nothing was proven"
    );
    assert!(
        torn > 0,
        "no kill landed inside a transaction ({ITERATIONS} tried) — the \
         invariant held, but only over completed batches, which proves \
         nothing about atomicity. Widen `kill_delay_ms` or raise BATCH."
    );
    eprintln!("{torn}/{ITERATIONS} kills landed inside a transaction");
}

/// A spread of kill delays, deterministic so a failure reproduces. Short
/// enough to stay inside the child's run, long enough that some land after a
/// batch has started.
fn kill_delay_ms(iteration: usize) -> u64 {
    // A small LCG over the iteration index — varied, and the same varied
    // sequence every run.
    let x = (iteration as u64)
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    25 + (x >> 33) % 200
}

/// The lowest `collection.id` the two disagree about, described — or `None`
/// if they agree everywhere.
///
/// Not `assert_eq!` on the two maps: these hold thousands of rows apiece, and
/// the dump of both is long enough to bury the one row that differs. A gate
/// whose failure cannot be read is a gate that gets rerun rather than
/// diagnosed.
fn first_disagreement(
    projected: &BTreeMap<i64, Value>,
    live: &BTreeMap<i64, Value>,
) -> Option<String> {
    let mut ids: Vec<i64> = projected.keys().chain(live.keys()).copied().collect();
    ids.sort_unstable();
    ids.dedup();

    let id = ids
        .into_iter()
        .find(|id| projected.get(id) != live.get(id))?;

    let describe = |row: Option<&Value>| match row {
        None => "absent".to_string(),
        Some(v) => serde_json::to_string(v).unwrap(),
    };
    Some(format!(
        "{} rows replayed, {} on disk; first disagreement is id {id}\n  \
         outbox says: {}\n  on disk:     {}",
        projected.len(),
        live.len(),
        describe(projected.get(&id)),
        describe(live.get(&id)),
    ))
}

/// How many of those rows the child has moved to `sold`. The half of the
/// state an insert/delete count cannot see.
fn live_sold(live: &BTreeMap<i64, Value>) -> usize {
    live.values().filter(|r| r["status"] == "sold").count()
}

/// The collection as it is on disk: `id -> the whole row as JSON`.
fn live_collection(conn: &Connection) -> BTreeMap<i64, Value> {
    let cols = columns(conn);
    let list = cols
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn
        .prepare(&format!("SELECT {list} FROM collection ORDER BY id"))
        .unwrap();
    let mut rows = stmt.query([]).unwrap();

    let mut out = BTreeMap::new();
    while let Some(row) = rows.next().unwrap() {
        let mut obj = Map::new();
        for (i, col) in cols.iter().enumerate() {
            obj.insert(col.clone(), to_json(row.get_ref(i).unwrap()));
        }
        let id = obj["id"].as_i64().unwrap();
        out.insert(id, Value::Object(obj));
    }
    out
}

/// The collection as the outbox says it is: every event from seq 1, applied
/// in order. This is exactly what the shipper will hand the tenant zone, so
/// comparing it to the table is comparing what the lakehouse would believe
/// against what is true.
fn replay_outbox(conn: &Connection) -> BTreeMap<i64, Value> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT seq, op, row_id, payload FROM {} ORDER BY seq",
            pkdump_db::outbox::TABLE
        ))
        .unwrap();
    let events = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    // A gap here would mean an event was lost rather than never written —
    // the failure mode the sequence number exists to make visible.
    for (i, (seq, ..)) in events.iter().enumerate() {
        assert_eq!(
            *seq,
            i as i64 + 1,
            "the outbox has a gap at seq {seq}: an event was lost"
        );
    }

    let mut state: BTreeMap<i64, Value> = BTreeMap::new();
    for (seq, op, row_id, payload) in events {
        let payload: Value = serde_json::from_str(&payload).unwrap();
        match op.as_str() {
            "insert" | "update" => {
                state.insert(row_id, payload);
            }
            "delete" => {
                assert!(
                    state.remove(&row_id).is_some(),
                    "seq {seq}: delete of a row the outbox never inserted"
                );
            }
            other => panic!("seq {seq}: unknown op '{other}'"),
        }
    }
    state
}

fn columns(conn: &Connection) -> Vec<String> {
    let mut stmt = conn.prepare("PRAGMA table_info(collection)").unwrap();
    let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
    rows.collect::<rusqlite::Result<_>>().unwrap()
}

/// The same mapping SQLite's own `json_object` applies, so a row read here
/// and the same row seen through a trigger compare equal. The child writes
/// no REAL and no BLOB, which keeps that a statement about integers, text
/// and NULL.
fn to_json(v: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Value::from(f),
        ValueRef::Text(t) => Value::from(std::str::from_utf8(t).unwrap()),
        ValueRef::Blob(_) => panic!("the child writes no BLOBs"),
    }
}

// ---------------------------------------------------------------------
// The child
// ---------------------------------------------------------------------

/// What the child's next batch does. One per outbox `op`.
enum Op {
    /// Acquire cards — `INSERT`.
    Acquire,
    /// Sell cards already held — `UPDATE`.
    Sell,
    /// Drop sold rows — `DELETE`.
    Delete,
}

fn count(conn: &Connection, sql: &str) -> usize {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap() as usize
}

/// Writes batches of collection mutations until something kills it.
///
/// Ignored, so it only runs when the parent asks for it by name — and it
/// returns immediately if [`DB_ENV`] is unset, so `cargo test -- --ignored`
/// does not start an unkillable loop.
#[test]
#[ignore = "the child process of a_killed_writer_never_leaves_..., not a test"]
fn the_child_that_gets_killed() {
    let Ok(db) = std::env::var(DB_ENV) else {
        return;
    };
    let marker = PathBuf::from(std::env::var("PKDUMP_OUTBOX_CRASH_MARKER").unwrap());
    let mut conn = pkdump_db::open_user(Path::new(&db)).unwrap();

    loop {
        let live = count(&conn, "SELECT count(*) FROM collection");
        let sold = count(
            &conn,
            "SELECT count(*) FROM collection WHERE status = 'sold'",
        );
        let owned = live - sold;

        // Three kinds of ownership change, and the replay has to survive
        // every one of them. Sell a batch when there are unsold rows to sell,
        // thin the sold ones out once they have piled up, and otherwise
        // acquire more — which keeps all three recurring for as long as the
        // child lives.
        //
        // The UPDATE leg is not decoration. An insert or a delete lost in a
        // crash shows up as a wrong row COUNT, which is the easy case; a
        // stale update payload leaves the counts identical and the row's
        // contents wrong, which is the failure the replay comparison exists
        // to catch and the only one the projection cannot survive silently.
        let op = if live >= BATCH * 3 && sold >= BATCH {
            Op::Delete
        } else if owned >= BATCH {
            Op::Sell
        } else {
            Op::Acquire
        };
        let (rows, sold) = match op {
            Op::Delete => (live - BATCH, sold - BATCH),
            Op::Sell => (live, sold + BATCH),
            Op::Acquire => (live + BATCH, sold),
        };
        std::fs::write(&marker, format!("{rows}:{sold}")).unwrap();

        let tx = conn.transaction().unwrap();
        match op {
            Op::Delete => {
                tx.execute(
                    "DELETE FROM collection WHERE id IN \
                     (SELECT id FROM collection WHERE status = 'sold' \
                      ORDER BY id LIMIT ?1)",
                    [BATCH],
                )
                .unwrap();
            }
            // Row at a time, like the insert leg, so a kill can land in the
            // middle of the batch rather than only between statements.
            Op::Sell => {
                let ids: Vec<i64> = tx
                    .prepare(
                        "SELECT id FROM collection WHERE status = 'owned' \
                         ORDER BY id LIMIT ?1",
                    )
                    .unwrap()
                    .query_map([BATCH], |r| r.get(0))
                    .unwrap()
                    .collect::<rusqlite::Result<_>>()
                    .unwrap();
                let mut stmt = tx
                    .prepare("UPDATE collection SET status = 'sold', notes = ?2 WHERE id = ?1")
                    .unwrap();
                for id in ids {
                    stmt.execute(rusqlite::params![
                        id,
                        format!("sold in the batch reaching {sold}")
                    ])
                    .unwrap();
                }
            }
            Op::Acquire => {
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO collection \
                           (printing_id, acquired_at, source, condition, status, notes) \
                         VALUES (?1, '2026-08-13T00:00:00Z', 'crash-test', \
                                 'Near Mint', 'owned', ?2)",
                    )
                    .unwrap();
                for i in 0..BATCH {
                    stmt.execute(rusqlite::params![
                        format!("p-{}-{i}", live),
                        format!("row {i} of the batch that reaches {rows}")
                    ])
                    .unwrap();
                }
            }
        }
        tx.commit().unwrap();

        // Only once the batch is durable does the in-flight marker go. A
        // marker the parent finds after the kill means a batch was in
        // flight when it landed.
        let _ = std::fs::remove_file(&marker);
    }
}
