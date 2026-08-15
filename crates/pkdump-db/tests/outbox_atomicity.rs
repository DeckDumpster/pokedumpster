//! The atomicity claim behind the ownership outbox (pd-5m54, extended to
//! sealed product by pd-4gop), proven the only way it can be: by killing a
//! writer mid-flight and looking at what survived.
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
//! replays the outbox from seq 1 and compares the result to the holdings
//! tables row for row. Every row. Every iteration.
//!
//! SIGKILL rather than a signal handler or a panic on purpose — it cannot be
//! caught, deferred or cleaned up after, so nothing in this process gets a
//! chance to tidy the two into agreement.
//!
//! **The child alternates between `collection` and `sealed_collection`**
//! (pd-4gop). Sealed holdings are a second source on the same outbox and the
//! same sequence, so the crash claim is about the pair of tables, not each
//! of them separately — and the projection is keyed on
//! `(source_table, row_id)`, because the two tables number their rows
//! independently and both start at 1. Keyed on `row_id` alone this gate
//! fails on the first iteration, which is the point of keying it that way.
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

/// Mutations per source per transaction in the child — so a transaction is
/// `BATCH * sources().len()` writes. Large enough that a batch takes long
/// enough to be killed in the middle of, small enough that plenty of them
/// commit.
const BATCH: usize = 200;

/// The holdings tables the outbox carries, read off the crate rather than
/// listed here: a source added to `SOURCE_TABLES` and not to this gate would
/// be a source whose atomicity nothing checks.
fn sources() -> Vec<&'static str> {
    pkdump_db::outbox::SOURCE_TABLES
        .iter()
        .map(|(t, _)| *t)
        .collect()
}

/// A holding, addressed the only way it can be: the table it lives in and
/// its id there. `collection` and `sealed_collection` number their rows
/// independently, so `row_id` alone names two different holdings.
type Holding = (String, i64);

// ---------------------------------------------------------------------
// The parent
// ---------------------------------------------------------------------

#[test]
fn a_killed_writer_never_leaves_the_holdings_and_the_outbox_disagreeing() {
    let exe = std::env::current_exe().expect("the test binary re-executes itself as the child");

    let mut torn = 0usize;
    let mut with_rows = 0usize;
    // Which sources actually got mutated somewhere across the whole run. A
    // gate that only ever exercised `collection` would pass with the sealed
    // triggers deleted outright.
    let mut sources_seen: BTreeMap<String, usize> = BTreeMap::new();

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
        let live = live_holdings(&conn);
        let projected = replay_outbox(&conn);

        if let Some(disagreement) = first_disagreement(&projected, &live) {
            panic!(
                "iteration {iteration}: the outbox replayed to different \
                 holdings than the ones on disk\n{disagreement}"
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
        for (source, _) in live.keys() {
            *sources_seen.entry(source.clone()).or_default() += 1;
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
    // The claim is about the holdings, both halves of them. A run that only
    // ever wrote singles proves nothing about the sealed triggers, and would
    // stay green if they were deleted.
    for source in sources() {
        assert!(
            sources_seen.contains_key(source),
            "no surviving {source} rows in {ITERATIONS} iterations — the \
             child never wrote that source, so its triggers were not tested"
        );
    }
    eprintln!(
        "{torn}/{ITERATIONS} kills landed inside a transaction; \
         surviving rows per source: {sources_seen:?}"
    );
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

/// The lowest holding the two disagree about, described — or `None` if they
/// agree everywhere.
///
/// Not `assert_eq!` on the two maps: these hold thousands of rows apiece, and
/// the dump of both is long enough to bury the one row that differs. A gate
/// whose failure cannot be read is a gate that gets rerun rather than
/// diagnosed.
fn first_disagreement(
    projected: &BTreeMap<Holding, Value>,
    live: &BTreeMap<Holding, Value>,
) -> Option<String> {
    let mut ids: Vec<Holding> = projected.keys().chain(live.keys()).cloned().collect();
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
        "{} rows replayed, {} on disk; first disagreement is {}.id {}\n  \
         outbox says: {}\n  on disk:     {}",
        projected.len(),
        live.len(),
        id.0,
        id.1,
        describe(projected.get(&id)),
        describe(live.get(&id)),
    ))
}

/// How many of those rows the child has moved to `sold`. The half of the
/// state an insert/delete count cannot see.
fn live_sold(live: &BTreeMap<Holding, Value>) -> usize {
    live.values().filter(|r| r["status"] == "sold").count()
}

/// The holdings as they are on disk, both sources: `(table, id) -> the whole
/// row as JSON`.
fn live_holdings(conn: &Connection) -> BTreeMap<Holding, Value> {
    let mut out = BTreeMap::new();
    for table in sources() {
        let cols = columns(conn, table);
        let list = cols
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let mut stmt = conn
            .prepare(&format!("SELECT {list} FROM \"{table}\" ORDER BY id"))
            .unwrap();
        let mut rows = stmt.query([]).unwrap();

        while let Some(row) = rows.next().unwrap() {
            let mut obj = Map::new();
            for (i, col) in cols.iter().enumerate() {
                obj.insert(col.clone(), to_json(row.get_ref(i).unwrap()));
            }
            let id = obj["id"].as_i64().unwrap();
            out.insert((table.to_string(), id), Value::Object(obj));
        }
    }
    out
}

/// The holdings as the outbox says they are: every event from seq 1, applied
/// in order. This is exactly what the shipper will hand the tenant zone, so
/// comparing it to the tables is comparing what the lakehouse would believe
/// against what is true.
///
/// Keyed on `(source_table, row_id)`. Both tables start their ids at 1, so a
/// projection keyed on `row_id` alone lets a sealed lot overwrite the single
/// that shares its number and a sealed delete remove it — which this gate
/// reports as a disagreement on the first iteration (pd-4gop).
fn replay_outbox(conn: &Connection) -> BTreeMap<Holding, Value> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT seq, source_table, op, row_id, payload FROM {} ORDER BY seq",
            pkdump_db::outbox::TABLE
        ))
        .unwrap();
    let events = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();

    // A gap here would mean an event was lost rather than never written —
    // the failure mode the sequence number exists to make visible. One
    // sequence over both sources, so this is a claim about the pair.
    for (i, (seq, ..)) in events.iter().enumerate() {
        assert_eq!(
            *seq,
            i as i64 + 1,
            "the outbox has a gap at seq {seq}: an event was lost"
        );
    }

    let mut state: BTreeMap<Holding, Value> = BTreeMap::new();
    for (seq, source, op, row_id, payload) in events {
        assert!(
            sources().contains(&source.as_str()),
            "seq {seq}: unknown source_table '{source}'"
        );
        let payload: Value = serde_json::from_str(&payload).unwrap();
        let key = (source, row_id);
        match op.as_str() {
            "insert" | "update" => {
                state.insert(key, payload);
            }
            "delete" => {
                assert!(
                    state.remove(&key).is_some(),
                    "seq {seq}: delete of a row the outbox never inserted"
                );
            }
            other => panic!("seq {seq}: unknown op '{other}'"),
        }
    }
    state
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .unwrap();
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
    /// Acquire holdings — `INSERT`.
    Acquire,
    /// Sell holdings already held — `UPDATE`.
    Sell,
    /// Drop sold rows — `DELETE`.
    Delete,
}

fn count(conn: &Connection, sql: &str) -> usize {
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap() as usize
}

/// `(rows, sold)` for one holdings table.
fn tally(conn: &Connection, table: &str) -> (usize, usize) {
    (
        count(conn, &format!("SELECT count(*) FROM \"{table}\"")),
        count(
            conn,
            &format!("SELECT count(*) FROM \"{table}\" WHERE status = 'sold'"),
        ),
    )
}

/// The `INSERT` a batch acquires with, for one source. The two tables take
/// different columns; everything else about a batch is the same.
fn acquire_sql(table: &str) -> &'static str {
    match table {
        "collection" => {
            "INSERT INTO collection \
               (printing_id, acquired_at, source, condition, status, notes) \
             VALUES (?1, '2026-08-13T00:00:00Z', 'crash-test', \
                     'Near Mint', 'owned', ?2)"
        }
        "sealed_collection" => {
            "INSERT INTO sealed_collection \
               (product_id, quantity, added_at, source, condition, status, notes) \
             VALUES (?1, 1, '2026-08-14T00:00:00Z', 'crash-test', \
                     'Near Mint', 'owned', ?2)"
        }
        other => panic!("no acquire statement for source '{other}'"),
    }
}

/// The catalog key a row of `table` is acquired against — text for singles
/// (`printing_id`), an integer for sealed (`product_id`). SQLite stores what
/// it is given, and the parent reads both back through the same mapping the
/// triggers' `json_object` used.
fn acquire_key(table: &str, n: usize) -> rusqlite::types::Value {
    if table == "collection" {
        format!("p-{n}").into()
    } else {
        (n as i64).into()
    }
}

/// Writes batches of holdings mutations until something kills it.
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
        // Every batch mutates BOTH sources inside ONE transaction (pd-4gop).
        // Not alternate batches: a kill has to be able to land between the
        // two tables' writes, which is precisely where a per-table outbox
        // would tear. It also keeps the two tables' ids in lockstep, so
        // every single has a sealed lot sharing its `row_id` and a
        // projection keyed on `row_id` alone cannot accidentally pass.
        let (live, sold) = tally(&conn, "collection");
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
        // The tables move in lockstep, so the totals the batch reaches are
        // one table's counts times the number of sources.
        let n = sources().len();
        std::fs::write(&marker, format!("{}:{}", rows * n, sold * n)).unwrap();

        let tx = conn.transaction().unwrap();
        for table in sources() {
            match op {
                Op::Delete => {
                    tx.execute(
                        &format!(
                            "DELETE FROM \"{table}\" WHERE id IN \
                             (SELECT id FROM \"{table}\" WHERE status = 'sold' \
                              ORDER BY id LIMIT ?1)"
                        ),
                        [BATCH],
                    )
                    .unwrap();
                }
                // Row at a time, like the insert leg, so a kill can land in
                // the middle of the batch rather than only between
                // statements.
                Op::Sell => {
                    let ids: Vec<i64> = tx
                        .prepare(&format!(
                            "SELECT id FROM \"{table}\" WHERE status = 'owned' \
                             ORDER BY id LIMIT ?1"
                        ))
                        .unwrap()
                        .query_map([BATCH], |r| r.get(0))
                        .unwrap()
                        .collect::<rusqlite::Result<_>>()
                        .unwrap();
                    let mut stmt = tx
                        .prepare(&format!(
                            "UPDATE \"{table}\" SET status = 'sold', notes = ?2 WHERE id = ?1"
                        ))
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
                    let mut stmt = tx.prepare(acquire_sql(table)).unwrap();
                    for i in 0..BATCH {
                        stmt.execute(rusqlite::params![
                            acquire_key(table, live + i),
                            format!("row {i} of the batch that reaches {rows}")
                        ])
                        .unwrap();
                    }
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
