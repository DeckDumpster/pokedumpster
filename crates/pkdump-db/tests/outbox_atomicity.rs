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

        assert_eq!(
            projected, live,
            "iteration {iteration}: the outbox replayed to a different \
             collection than the one on disk"
        );

        // --- and the evidence that it was worth asserting -------------
        // `in-flight` holds the row count the batch being written would have
        // reached. It survives the kill because the child never gets to
        // remove it.
        if let Ok(target) = std::fs::read_to_string(&marker) {
            let target: usize = target.trim().parse().unwrap();
            if live.len() != target {
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
        let live: usize = conn
            .query_row("SELECT count(*) FROM collection", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap() as usize;

        // Enough rows to be worth thinning: delete a batch. Otherwise add
        // one. Both are ownership changes; the replay has to survive either.
        let deleting = live >= BATCH * 3;
        let target = if deleting { live - BATCH } else { live + BATCH };
        std::fs::write(&marker, target.to_string()).unwrap();

        let tx = conn.transaction().unwrap();
        if deleting {
            tx.execute(
                "DELETE FROM collection WHERE id IN \
                 (SELECT id FROM collection ORDER BY id LIMIT ?1)",
                [BATCH],
            )
            .unwrap();
        } else {
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
                    format!("row {i} of the batch that reaches {target}")
                ])
                .unwrap();
            }
        }
        tx.commit().unwrap();

        // Only once the batch is durable does the in-flight marker go. A
        // marker the parent finds after the kill means a batch was in
        // flight when it landed.
        let _ = std::fs::remove_file(&marker);
    }
}
