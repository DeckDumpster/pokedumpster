//! The ownership outbox — the record of holdings changes leaving a
//! collection, written atomically with the change that caused it (pd-5m54).
//!
//! There is deliberately almost no code here. The table and its writer are
//! declared in `schema_user.sql`: three `AFTER INSERT/UPDATE/DELETE ON
//! collection` triggers that append one row each. A trigger fires inside the
//! statement's own transaction, so a holding cannot change without the event
//! being recorded and an event cannot survive a mutation that rolled back —
//! atomicity by construction, with no instant in between for a crash to land
//! in and no call site that has to remember to write.
//!
//! ## What the contract is
//!
//! * Every `collection` mutation appends exactly one row, in the same
//!   transaction as the mutation.
//! * `seq` is monotonic, gap-free and never reused — a missing number means
//!   an event was LOST, which is what makes the offline side's consistency
//!   provable rather than assumed.
//! * `payload` is the whole row as a JSON object — the post-image for
//!   `insert`/`update`, the pre-image for `delete`.
//!
//! ## What it is not
//!
//! It is not the tenant's collection state, and so it is not part of the
//! portable JSON envelope (`crate::json_backup`). It is the log of changes
//! *leaving* the collection: an envelope that carried it would restore
//! already-shipped events into a fresh database, and a restore already
//! records itself here — its deletes and its inserts fire the triggers like
//! any other write.
//!
//! # Emitting current state — backfill, redrive, DR reconcile (pd-385w)
//!
//! The triggers only fire on FUTURE mutations. On a collection that already
//! holds cards when they are created, every existing holding generated no
//! event and never will — so a shipper armed against that outbox would fill
//! the tenant zone with post-deployment changes only, and every valuation
//! computed from it would under-report, silently. [`emit`] closes that.
//!
//! It is **one operation over a [`Scope`]**, not three tools, because the
//! rare uses run under pressure after something is already broken:
//!
//! | use | scope |
//! |---|---|
//! | backfill | [`Scope::Collection`] — every row that predates the triggers |
//! | redrive | [`Scope::Seq`] / [`Scope::Row`] — after the shipper lost data |
//! | DR reconcile | [`Scope::Collection`] — after a restore to an earlier point |
//!
//! A backfill that shares its code with the everyday path has been exercised
//! every day; a separate `--repair` script has been exercised never.
//!
//! ## The four rules it obeys
//!
//! 1. **Through the outbox, never straight to the zone.** [`emit`] appends
//!    ordinary outbox rows and touches nothing else. Two writers with
//!    different code paths would mean the rare one is untested, and the zone
//!    could then disagree with the outbox with nothing able to detect it.
//! 2. **Provenance without different handling.** Every event carries
//!    [`Source`] — `trigger`, `backfill` or `redrive`. **Consumers must not
//!    branch on it.** The moment the shipper reads it, backfill stops being
//!    the same path.
//! 3. **Last-write-wins by `occurred_at`, tie-broken by `seq`** — the
//!    resolution rule, implemented once in [`project`]. An emitted event
//!    carries **the row's own last-known change time**, never the moment it
//!    was re-emitted, which is what makes a stale snapshot lose to a newer
//!    live mutation instead of clobbering it. See [`emit`].
//! 4. **Re-running is safe but not silent.** Every run lands a row in
//!    `ownership_emit_log`; a second full backfill without `force` is
//!    refused, naming when the first completed.
//!
//! ## Why replay is idempotent at all
//!
//! Because `payload` is the whole row rather than a delta. Applying the same
//! event twice is an upsert to the same value, where `+1` applied twice is a
//! corruption. Shrinking the payload to a delta would destroy backfill,
//! redrive and DR reconcile together — it is the single decision this
//! module rests on.

use std::collections::BTreeMap;

use rusqlite::{Connection, Transaction};

use crate::{DbError, Result};

/// The outbox table's name. The one place it is spelled in Rust.
pub const TABLE: &str = "ownership_outbox";

/// The emit ledger's name — rule 4's record that a backfill has run.
pub const EMIT_LOG: &str = "ownership_emit_log";

/// Neither of these is collection state: they are the log of changes
/// *leaving* a collection and the record of who re-emitted them. Both are
/// absent from the portable JSON envelope in both directions
/// (`crate::json_backup`).
pub const TRANSPORT_TABLES: &[&str] = &[TABLE, EMIT_LOG];

/// The holdings tables the outbox carries, each with the column that dates
/// a row which has never emitted an event.
///
/// **This list is the emitter's whole notion of scope, and it is checked
/// against the triggers rather than trusted** — `every_triggered_table_is_
/// emittable` reads `sqlite_master` for the outbox triggers and asserts the
/// tables they fire on are exactly these. So the day `sealed_collection`
/// gains its triggers (pd-4gop settled that sealed product is a holding
/// like any other), that test goes RED until this list grows the entry.
/// Backfilling singles and quietly missing sealed would mean a second
/// backfill pass later, and nothing would have said so.
///
/// The payload itself is NOT listed here — it is read from
/// `pragma_table_info`, so an emitted event carries exactly the columns the
/// table declares, in declaration order, which is what the triggers' own
/// hand-written `json_object` lists produce. The two agree by construction
/// rather than by a second list to keep in step.
pub const SOURCE_TABLES: &[(&str, &str)] = &[("collection", "acquired_at")];

/// The `occurred_at` given to a row with no event and no usable timestamp
/// of its own. Deliberately the earliest representable instant: an event
/// dated here loses to everything, which is the safe direction for a value
/// that means "we do not know when this row last changed".
const UNDATED: &str = "0001-01-01T00:00:00.000Z";

/// Where an outbox event came from. Rule 2: it is recorded, and it does not
/// change how the event is handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A trigger, firing inside the mutation's own transaction.
    Trigger,
    /// [`emit`] over [`Scope::Collection`].
    Backfill,
    /// [`emit`] over [`Scope::Seq`] or [`Scope::Row`].
    Redrive,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Trigger => "trigger",
            Source::Backfill => "backfill",
            Source::Redrive => "redrive",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an [`emit`] run covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every current row of every holdings table — the backfill, and the
    /// DR reconcile.
    Collection,
    /// The rows named by outbox events in `from..=to` — the redrive of a
    /// slice the shipper lost.
    ///
    /// This reads the range out of the outbox, so it can only name rows
    /// whose events are still there. A range the shipper has already
    /// trimmed names nothing, and the honest answer then is
    /// [`Scope::Collection`] — a reconcile of the whole collection — or
    /// [`Scope::Row`] for holdings identified some other way. Said out loud
    /// because a scope that silently emits nothing looks exactly like a
    /// scope with nothing to do.
    Seq { from: i64, to: i64 },
    /// One row, by `collection.id`.
    Row(i64),
}

impl Scope {
    /// How this scope is written in the ledger and in operator output.
    pub fn label(&self) -> String {
        match self {
            Scope::Collection => "collection".to_string(),
            Scope::Seq { from, to } => format!("seq:{from}..{to}"),
            Scope::Row(id) => format!("row:{id}"),
        }
    }

    /// The provenance a run over this scope writes. Derived from the scope
    /// rather than asked for: an operator naming their own audit label is
    /// an operator who can get it wrong, and the whole value of the column
    /// is that it is honest.
    pub fn provenance(&self) -> Source {
        match self {
            Scope::Collection => Source::Backfill,
            Scope::Seq { .. } | Scope::Row(_) => Source::Redrive,
        }
    }

    fn validate(&self) -> Result<()> {
        match self {
            Scope::Seq { from, to } if from > to => Err(DbError::Invalid(format!(
                "seq range {from}..{to} runs backwards"
            ))),
            Scope::Seq { from, .. } if *from < 1 => Err(DbError::Invalid(format!(
                "seq numbering starts at 1, so {from} names no event"
            ))),
            _ => Ok(()),
        }
    }
}

/// What one [`emit`] run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emitted {
    pub scope: String,
    pub source: Source,
    pub events: usize,
    /// The `seq` range this run appended, or `None` when it emitted nothing
    /// — a redrive of a range whose rows are all still shipped, say.
    pub seq_first: Option<i64>,
    pub seq_last: Option<i64>,
    /// Events per holdings table, in [`SOURCE_TABLES`] order. What makes a
    /// run that covered singles and missed sealed visible in its own output
    /// rather than only in a later reconciliation.
    pub per_table: Vec<(String, usize)>,
}

/// A row of `ownership_emit_log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmitRun {
    pub id: i64,
    pub scope: String,
    pub source: String,
    pub completed_at: String,
    pub rows_emitted: i64,
    pub seq_first: Option<i64>,
    pub seq_last: Option<i64>,
    pub forced: bool,
}

/// One outbox event, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: i64,
    pub occurred_at: String,
    pub source_table: String,
    pub op: String,
    pub row_id: i64,
    pub payload: String,
    pub source: String,
}

// ---------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------

/// Every event in the outbox, in `seq` order.
pub fn events(conn: &Connection) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT seq, occurred_at, source_table, op, row_id, payload, source \
         FROM {TABLE} ORDER BY seq"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(Event {
            seq: r.get(0)?,
            occurred_at: r.get(1)?,
            source_table: r.get(2)?,
            op: r.get(3)?,
            row_id: r.get(4)?,
            payload: r.get(5)?,
            source: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// **Rule 3, and the only implementation of it**: reduce an event stream to
/// the holdings it describes, resolving conflicts by `occurred_at` and
/// tie-breaking by `seq`.
///
/// This is what the tenant zone holds, and it is deliberately a pure
/// function over events so that "the zone rebuilt by backfill equals the
/// zone built incrementally" is a claim that can be *made* — the shipper
/// (pd-dxn3) writes this projection, and both sides of that comparison run
/// the same reduction.
///
/// ## Why `occurred_at` and not `seq`
///
/// A redrive takes a snapshot and appends it with a NEW, higher `seq`. If a
/// live mutation lands in between, resolving by `seq` alone would let the
/// stale snapshot overwrite newer truth. Resolving by `occurred_at` cannot,
/// because [`emit`] dates its events with the row's own last-known change
/// time rather than the moment of re-emission. `seq` breaks exact ties,
/// where the two events describe the same instant and the later one is by
/// definition no staler.
///
/// A `delete` removes the holding; `insert` and `update` are the same
/// upsert, because the payload is the whole row in both cases.
pub fn project(events: &[Event]) -> BTreeMap<(String, i64), serde_json::Value> {
    let mut ordered: Vec<&Event> = events.iter().collect();
    ordered.sort_by(|a, b| {
        a.occurred_at
            .cmp(&b.occurred_at)
            .then_with(|| a.seq.cmp(&b.seq))
    });

    let mut state = BTreeMap::new();
    for e in ordered {
        let key = (e.source_table.clone(), e.row_id);
        if e.op == "delete" {
            state.remove(&key);
        } else {
            // A payload that will not parse is a corrupted event, and a
            // projection that silently skipped it would under-report a
            // holding — the exact failure the whole item exists to prevent.
            let value = serde_json::from_str(&e.payload)
                .unwrap_or_else(|_| serde_json::Value::String(e.payload.clone()));
            state.insert(key, value);
        }
    }
    state
}

/// Every emit run this collection has recorded, newest first.
pub fn runs(conn: &Connection) -> Result<Vec<EmitRun>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, scope, source, completed_at, rows_emitted, seq_first, seq_last, forced \
         FROM {EMIT_LOG} ORDER BY id DESC"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok(EmitRun {
            id: r.get(0)?,
            scope: r.get(1)?,
            source: r.get(2)?,
            completed_at: r.get(3)?,
            rows_emitted: r.get(4)?,
            seq_first: r.get(5)?,
            seq_last: r.get(6)?,
            forced: r.get::<_, i64>(7)? != 0,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The most recent full backfill of this collection, if there has been one.
pub fn last_backfill(conn: &Connection) -> Result<Option<EmitRun>> {
    Ok(runs(conn)?
        .into_iter()
        .find(|r| r.source == Source::Backfill.as_str()))
}

// ---------------------------------------------------------------------
// Emitting
// ---------------------------------------------------------------------

/// Emit the current state of `scope` as outbox events.
///
/// The whole run — every event and the ledger row describing them — lands in
/// one transaction, so the ledger cannot record a backfill whose events are
/// not there.
///
/// ## What an emitted event carries
///
/// * `op` is `insert` for a row that exists. A row the scope names that is
///   *gone* re-emits its last-known `delete` instead, so a redrive of a
///   lost slice reproduces the removals in it and not only the survivors.
///   [`Scope::Collection`] does not do this — a backfill's job is "these
///   are the rows", and re-emitting every historical delete would grow the
///   outbox with events the rebuilt zone has no use for.
/// * `payload` is the whole row, built from `pragma_table_info`, so it is
///   byte-identical to what the trigger would have written.
/// * `occurred_at` is **the row's own last-known change time**: the newest
///   `occurred_at` the outbox already holds for it, else the row's own
///   timestamp column, else [`UNDATED`]. Never the moment of emission —
///   that is rule 3, and dating an emitted event `now` is precisely how a
///   stale snapshot comes to clobber a live mutation.
/// * `source` is [`Scope::provenance`].
///
/// ## The snapshot bound
///
/// Every read is bounded to the `max(seq)` observed when the transaction
/// opened. Without it the run's own events would feed back into the
/// `occurred_at` lookup, and an emitted event would date itself from the
/// event it just wrote.
///
/// ## Rule 4
///
/// A second [`Scope::Collection`] run refuses unless `force`, naming when
/// the first completed. Replay is idempotent, so the refusal is not about
/// correctness — it is about a backfill re-run by accident at 3am being a
/// decision rather than a shrug.
pub fn emit(conn: &mut Connection, scope: &Scope, force: bool) -> Result<Emitted> {
    scope.validate()?;
    let source = scope.provenance();

    if *scope == Scope::Collection
        && !force
        && let Some(prior) = last_backfill(conn)?
    {
        return Err(DbError::Conflict(format!(
            "this collection was already backfilled at {} ({} events, {}). \
                 Running it again is SAFE — every event carries the whole row, so \
                 replaying one is an upsert to the same value — but it is not \
                 something to do by accident. Pass --force to do it anyway.",
            prior.completed_at, prior.rows_emitted, prior.scope,
        )));
    }

    let tx = conn.transaction()?;
    let snapshot: i64 = tx.query_row(
        &format!("SELECT coalesce(max(seq), 0) FROM {TABLE}"),
        [],
        |r| r.get(0),
    )?;

    let mut per_table = Vec::new();
    for (table, ts_col) in SOURCE_TABLES {
        let mut n = emit_current_rows(&tx, table, ts_col, scope, source, snapshot)?;
        if *scope != Scope::Collection {
            n += emit_missing_rows(&tx, table, scope, source, snapshot)?;
        }
        per_table.push(((*table).to_string(), n));
    }
    let events: usize = per_table.iter().map(|(_, n)| n).sum();

    // The range is derived from the last number allocated, counting BACK by
    // the events written — never from `snapshot + 1`. `seq` is AUTOINCREMENT
    // precisely so a number is never reused after the shipper trims a
    // shipped prefix, so on a trimmed outbox the highest number PRESENT and
    // the last number ALLOCATED are different values, and a range reported
    // from the former names events that do not exist. Nothing else allocates
    // an outbox number inside this transaction, so the run's own events are
    // contiguous and this is exact.
    let (seq_first, seq_last) = if events == 0 {
        (None, None)
    } else {
        let last: i64 = tx.query_row(&format!("SELECT max(seq) FROM {TABLE}"), [], |r| r.get(0))?;
        (Some(last - events as i64 + 1), Some(last))
    };

    tx.execute(
        &format!(
            "INSERT INTO {EMIT_LOG} \
                 (scope, source, completed_at, rows_emitted, seq_first, seq_last, forced) \
             VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), ?3, ?4, ?5, ?6)"
        ),
        rusqlite::params![
            scope.label(),
            source.as_str(),
            events as i64,
            seq_first,
            seq_last,
            force as i64,
        ],
    )?;
    tx.commit()?;

    Ok(Emitted {
        scope: scope.label(),
        source,
        events,
        seq_first,
        seq_last,
        per_table,
    })
}

/// `json_object('c1', t."c1", …)` over every column `table` declares, in
/// declaration order — the same list, from the same source, that
/// `outbox.rs`'s payload gate holds the triggers to.
fn payload_expr(tx: &Transaction, table: &str) -> Result<String> {
    let mut stmt = tx.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let cols: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<rusqlite::Result<_>>()?;
    if cols.is_empty() {
        return Err(DbError::Env(format!("{table} declares no columns")));
    }
    let pairs: Vec<String> = cols
        .iter()
        .map(|c| format!("'{c}', t.\"{c}\""))
        .collect::<Vec<_>>();
    Ok(format!("json_object({})", pairs.join(", ")))
}

/// `(sql, params)` restricting a row id to the scope. `Collection` places
/// no restriction: the table's own rows are the scope.
fn scope_predicate(
    table: &str,
    scope: &Scope,
    snapshot: i64,
    id_expr: &str,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    match scope {
        Scope::Collection => ("1".to_string(), vec![]),
        Scope::Row(id) => (format!("{id_expr} = ?"), vec![Value::Integer(*id)]),
        Scope::Seq { from, to } => (
            format!(
                "{id_expr} IN (SELECT row_id FROM {TABLE} \
                 WHERE source_table = ? AND seq BETWEEN ? AND ? AND seq <= ?)"
            ),
            vec![
                Value::Text(table.to_string()),
                Value::Integer(*from),
                Value::Integer(*to),
                Value::Integer(snapshot),
            ],
        ),
    }
}

/// The row's own last-known change time — see [`emit`]. `strftime` both
/// normalises the fallback column into the outbox's own format and rejects
/// anything unparseable by returning NULL, which falls through to
/// [`UNDATED`] rather than putting an unorderable string in the column the
/// whole resolution rule sorts on.
fn occurred_at_expr(ts_col: &str) -> String {
    format!(
        "coalesce( \
           (SELECT max(o.occurred_at) FROM {TABLE} o \
             WHERE o.source_table = ?1 AND o.row_id = t.id AND o.seq <= ?2), \
           strftime('%Y-%m-%dT%H:%M:%fZ', t.\"{ts_col}\"), \
           '{UNDATED}')"
    )
}

fn emit_current_rows(
    tx: &Transaction,
    table: &str,
    ts_col: &str,
    scope: &Scope,
    source: Source,
    snapshot: i64,
) -> Result<usize> {
    let (predicate, mut params) = scope_predicate(table, scope, snapshot, "t.id");
    let sql = format!(
        "INSERT INTO {TABLE} (occurred_at, source_table, op, row_id, payload, source) \
         SELECT {occurred}, ?1, 'insert', t.id, {payload}, ?3 \
         FROM \"{table}\" t WHERE {predicate} ORDER BY t.id",
        occurred = occurred_at_expr(ts_col),
        payload = payload_expr(tx, table)?,
    );
    let mut bound: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(table.to_string()),
        rusqlite::types::Value::Integer(snapshot),
        rusqlite::types::Value::Text(source.as_str().to_string()),
    ];
    bound.append(&mut params);
    Ok(tx.execute(&sql, rusqlite::params_from_iter(bound))?)
}

/// Re-emit the last-known `delete` for a row the scope names that is no
/// longer in the table. A redrive exists because a slice of the stream was
/// lost; if that slice removed a holding, replaying only the survivors
/// leaves the zone holding a card the tenant does not own.
fn emit_missing_rows(
    tx: &Transaction,
    table: &str,
    scope: &Scope,
    source: Source,
    snapshot: i64,
) -> Result<usize> {
    let (predicate, mut params) = scope_predicate(table, scope, snapshot, "o.row_id");
    let sql = format!(
        "INSERT INTO {TABLE} (occurred_at, source_table, op, row_id, payload, source) \
         SELECT o.occurred_at, o.source_table, 'delete', o.row_id, o.payload, ?3 \
         FROM {TABLE} o \
         WHERE o.source_table = ?1 \
           AND o.seq <= ?2 \
           AND o.op = 'delete' \
           AND o.seq = (SELECT max(o2.seq) FROM {TABLE} o2 \
                         WHERE o2.source_table = o.source_table \
                           AND o2.row_id = o.row_id AND o2.seq <= ?2) \
           AND NOT EXISTS (SELECT 1 FROM \"{table}\" t WHERE t.id = o.row_id) \
           AND {predicate} \
         ORDER BY o.row_id",
    );
    let mut bound: Vec<rusqlite::types::Value> = vec![
        rusqlite::types::Value::Text(table.to_string()),
        rusqlite::types::Value::Integer(snapshot),
        rusqlite::types::Value::Text(source.as_str().to_string()),
    ];
    bound.append(&mut params);
    Ok(tx.execute(&sql, rusqlite::params_from_iter(bound))?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbError, collection, connect_user};
    use rusqlite::Connection;

    /// A user connection whose catalog holds two printings of one card and
    /// one printing of another — enough to exercise `change_printing` in
    /// both the allowed and the refused direction.
    fn user_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = crate::open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-2', 'sv3pt5', '2', 2, 'Ivysaur')",
                [],
            )
            .unwrap();
            for (printing, card, variant) in [
                ("sv3pt5-1-normal", "sv3pt5-1", "normal"),
                ("sv3pt5-1-holo", "sv3pt5-1", "holo"),
                ("sv3pt5-2-normal", "sv3pt5-2", "normal"),
            ] {
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) \
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![printing, card, variant],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn new_copy(printing_id: &str) -> collection::NewCopy {
        collection::NewCopy {
            printing_id: printing_id.to_string(),
            source: "test".to_string(),
            ..Default::default()
        }
    }

    /// Every outbox row, in sequence order.
    fn evs(conn: &Connection) -> Vec<Event> {
        events(conn).unwrap()
    }

    fn ops(conn: &Connection) -> Vec<String> {
        evs(conn).into_iter().map(|e| e.op).collect()
    }

    fn count(conn: &Connection) -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {TABLE}"), [], |r| r.get(0))
            .unwrap()
    }

    // -----------------------------------------------------------------
    // The shape of an event
    // -----------------------------------------------------------------

    #[test]
    fn adding_a_copy_appends_one_insert_event_carrying_the_new_row() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();

        let events = evs(&conn);
        assert_eq!(events.len(), 1, "one mutation, one event");
        let e = &events[0];
        assert_eq!(e.seq, 1);
        assert_eq!(e.op, "insert");
        assert_eq!(e.row_id, id);

        let payload: serde_json::Value = serde_json::from_str(&e.payload).unwrap();
        assert_eq!(payload["id"], id);
        assert_eq!(payload["printing_id"], "sv3pt5-1-normal");
        assert_eq!(payload["status"], "owned");
        assert_eq!(payload["condition"], "Near Mint");
        assert!(payload["notes"].is_null(), "an unset column is JSON null");
    }

    #[test]
    fn a_delete_event_carries_the_row_as_it_was() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        assert!(collection::delete(&conn, id).unwrap());

        let events = evs(&conn);
        assert_eq!(ops(&conn), ["insert", "delete"]);
        let payload: serde_json::Value = serde_json::from_str(&events[1].payload).unwrap();
        assert_eq!(
            payload["printing_id"], "sv3pt5-1-holo",
            "a delete records the pre-image — the row is gone, so nothing \
             else can say what was lost"
        );
        assert_eq!(events[1].row_id, id);
    }

    /// The gate that stops a column being added to `collection` and quietly
    /// missing from the event. The payload is a hand-written `json_object`
    /// in three triggers — SQLite has no `NEW.*` — so nothing but this
    /// comparison keeps the list honest.
    ///
    /// Seen red: dropping `'grade_cert', NEW.grade_cert` from the insert
    /// trigger fails it with `payload is missing: ["grade_cert"]`.
    #[test]
    fn every_column_of_collection_reaches_the_payload() {
        let (_dir, mut conn) = user_conn();

        let declared: Vec<String> = {
            let mut stmt = conn.prepare("PRAGMA table_info(collection)").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };

        // Every op, so a column added to one trigger and not the others is
        // caught too.
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        collection::set_status(&mut conn, id, "sold", None).unwrap();
        collection::delete(&conn, id).unwrap();

        let events = evs(&conn);
        assert_eq!(events.len(), 3);
        for e in &events {
            let (seq, op) = (e.seq, &e.op);
            let payload: serde_json::Value = serde_json::from_str(&e.payload).unwrap();
            let carried: Vec<&String> = payload.as_object().unwrap().keys().collect();
            let missing: Vec<&String> = declared.iter().filter(|c| !carried.contains(c)).collect();
            assert!(
                missing.is_empty(),
                "{op} event (seq {seq}) payload is missing: {missing:?}"
            );
            let extra: Vec<&&String> = carried
                .iter()
                .filter(|k| !declared.iter().any(|c| c == **k))
                .collect();
            assert!(
                extra.is_empty(),
                "{op} event (seq {seq}) payload invents: {extra:?}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Every write path, whether or not it knows the outbox exists
    // -----------------------------------------------------------------

    /// The reason the writer is a trigger and not a call site. Each of these
    /// is a separate function in `collection.rs`; none of them mentions the
    /// outbox.
    #[test]
    fn every_collection_mutation_appends_an_event() {
        let (_dir, mut conn) = user_conn();

        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        assert_eq!(count(&conn), 1, "add");

        collection::update(
            &conn,
            id,
            &collection::CopyEdit {
                condition: Some("Lightly Played".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(count(&conn), 2, "update");

        collection::set_status(&mut conn, id, "listed", None).unwrap();
        assert_eq!(count(&conn), 3, "set_status");

        let binder = crate::binders::create(
            &conn,
            &crate::binders::NewBinder {
                name: "b".into(),
                ..Default::default()
            },
        )
        .unwrap();
        collection::move_to(&mut conn, id, Some(binder), None, None).unwrap();
        assert_eq!(count(&conn), 4, "move_to");

        collection::change_printing(&conn, id, "sv3pt5-1-holo").unwrap();
        assert_eq!(count(&conn), 5, "change_printing");

        assert!(collection::delete_latest_for_printing(&conn, "sv3pt5-1-holo").unwrap());
        assert_eq!(count(&conn), 6, "delete_latest_for_printing");

        assert_eq!(
            ops(&conn),
            ["insert", "update", "update", "update", "update", "delete"]
        );
    }

    /// The paths that write `collection` in raw SQL — the order importer,
    /// the CSV importer, the JSON restore, the fixture seeder — never call
    /// `collection::add`. A writer bolted onto the service functions would
    /// miss all of them; a trigger cannot.
    #[test]
    fn a_raw_sql_writer_that_never_heard_of_the_outbox_is_covered_anyway() {
        let (_dir, conn) = user_conn();
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source) \
             VALUES ('sv3pt5-2-normal', '2026-08-13T00:00:00Z', 'order_import')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE collection SET notes = 'x'", [])
            .unwrap();
        conn.execute("DELETE FROM collection", []).unwrap();
        assert_eq!(ops(&conn), ["insert", "update", "delete"]);
    }

    /// Deleting a binder sets `collection.binder_id` to NULL through
    /// `ON DELETE SET NULL` — SQLite mutating a holding, with no Rust
    /// anywhere in the write. A writer bolted onto `binders::delete` would
    /// have to know that a foreign key elsewhere touches the collection; the
    /// trigger is on the table the change lands in, so it does not.
    ///
    /// (The copy is added straight into the binder rather than moved into
    /// it, because a `move_to` leaves a `movement_log` row whose own foreign
    /// key refuses the binder delete outright — pd-hj2w.)
    #[test]
    fn a_foreign_key_cascade_is_a_mutation_too() {
        let (_dir, mut conn) = user_conn();
        let binder = crate::binders::create(
            &conn,
            &crate::binders::NewBinder {
                name: "b".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let id = collection::add(
            &mut conn,
            &collection::NewCopy {
                binder_id: Some(binder),
                ..new_copy("sv3pt5-1-normal")
            },
        )
        .unwrap();
        assert_eq!(count(&conn), 1);

        assert!(crate::binders::delete(&conn, binder).unwrap());

        let events = evs(&conn);
        assert_eq!(ops(&conn), ["insert", "update"]);
        assert_eq!(events[1].row_id, id);
        let payload: serde_json::Value = serde_json::from_str(&events[1].payload).unwrap();
        assert!(
            payload["binder_id"].is_null(),
            "the event carries the holding as the cascade left it"
        );
    }

    // -----------------------------------------------------------------
    // Atomicity
    // -----------------------------------------------------------------

    /// The in-process half of the atomicity claim: a mutation that does not
    /// commit leaves no event. (The out-of-process half — SIGKILL between
    /// the two — is `tests/outbox_atomicity.rs`.)
    #[test]
    fn a_rolled_back_mutation_leaves_no_event() {
        let (_dir, mut conn) = user_conn();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO collection (printing_id, acquired_at, source) \
             VALUES ('sv3pt5-1-normal', '2026-08-13T00:00:00Z', 'test')",
            [],
        )
        .unwrap();
        assert_eq!(
            count(&tx),
            1,
            "inside the transaction the event is already there"
        );
        tx.rollback().unwrap();

        assert_eq!(count(&conn), 0, "the event rolled back with the mutation");
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    /// A rollback must not burn a sequence number. AUTOINCREMENT keeps its
    /// counter in `sqlite_sequence`, which is an ordinary table and rolls
    /// back with everything else — so the shipper's "a gap means a lost
    /// event" reading survives a failed write. Asserted rather than assumed:
    /// if this ever stopped holding, gap detection would report phantoms.
    #[test]
    fn a_rolled_back_mutation_does_not_burn_a_sequence_number() {
        let (_dir, mut conn) = user_conn();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO collection (printing_id, acquired_at, source) \
             VALUES ('sv3pt5-1-normal', '2026-08-13T00:00:00Z', 'test')",
            [],
        )
        .unwrap();
        tx.rollback().unwrap();

        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        assert_eq!(evs(&conn)[0].seq, 1, "the first surviving event is seq 1");
    }

    /// A mutation refused *after* it has written — `move_to` rejects a
    /// binder+deck conflict before touching anything, but `change_printing`
    /// to another card's printing is refused after the row is read, and the
    /// FK check on commit is later still. Whatever the refusal, the pair
    /// stays consistent.
    #[test]
    fn a_refused_mutation_leaves_neither_a_change_nor_an_event() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();

        let err = collection::change_printing(&conn, id, "sv3pt5-2-normal").unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)), "{err:?}");

        assert_eq!(count(&conn), 1, "only the add");
        let printing: String = conn
            .query_row(
                "SELECT printing_id FROM collection WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(printing, "sv3pt5-1-normal");
    }

    // -----------------------------------------------------------------
    // The sequence
    // -----------------------------------------------------------------

    /// SQLite serialises writers, so "concurrent" here means what it means
    /// in production: several connections to one file, each waiting its turn
    /// through the busy timeout. Every event must still land on its own
    /// number, in order, with no gaps and no reuse.
    #[test]
    fn the_sequence_is_monotonic_and_gap_free_under_concurrent_writers() {
        const WRITERS: usize = 4;
        const EACH: usize = 25;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        crate::open_user(&path).unwrap();

        std::thread::scope(|s| {
            for w in 0..WRITERS {
                let path = path.clone();
                s.spawn(move || {
                    let conn = crate::open_user(&path).unwrap();
                    for i in 0..EACH {
                        conn.execute(
                            "INSERT INTO collection (printing_id, acquired_at, source) \
                             VALUES (?1, '2026-08-13T00:00:00Z', 'test')",
                            [format!("p-{w}-{i}")],
                        )
                        .unwrap();
                    }
                });
            }
        });

        let conn = crate::open_user(&path).unwrap();
        let seqs: Vec<i64> = evs(&conn).into_iter().map(|e| e.seq).collect();
        assert_eq!(seqs.len(), WRITERS * EACH);
        assert_eq!(
            seqs,
            (1..=(WRITERS * EACH) as i64).collect::<Vec<_>>(),
            "every event on its own number, in order, no gaps"
        );
    }

    /// The shipper will trim a shipped prefix. The numbers it trimmed must
    /// never come round again — a reused `seq` is an event silently
    /// overwritten in the tenant zone.
    #[test]
    fn a_trimmed_sequence_number_is_never_reused() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        conn.execute(&format!("DELETE FROM {TABLE}"), []).unwrap();

        collection::add(&mut conn, &new_copy("sv3pt5-2-normal")).unwrap();
        assert_eq!(
            evs(&conn)[0].seq,
            3,
            "AUTOINCREMENT, so the next event continues past the trimmed rows"
        );
    }

    // -----------------------------------------------------------------
    // Not collection state
    // -----------------------------------------------------------------

    #[test]
    fn the_outbox_is_not_carried_by_the_json_envelope() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        assert_eq!(count(&conn), 1);

        let envelope: serde_json::Value =
            serde_json::from_str(&crate::json_backup::export(&conn).unwrap()).unwrap();
        assert!(
            envelope.get(TABLE).is_none(),
            "the envelope is collection state; the outbox is the log of \
             changes leaving it"
        );
        assert!(
            envelope.get("collection").is_some(),
            "...and the collection itself is still there"
        );
    }

    /// A restore is a mutation like any other: it empties the collection and
    /// fills it again, and both halves are recorded. The outbox that results
    /// describes the restored state — it is not the envelope's outbox,
    /// because the envelope has none.
    #[test]
    fn a_restore_records_itself_rather_than_restoring_a_stale_log() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        let envelope = crate::json_backup::export(&conn).unwrap();

        collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        let before = count(&conn);
        assert_eq!(before, 2);

        crate::json_backup::import(
            &mut conn,
            &envelope,
            crate::json_backup::OnExisting::Replace,
        )
        .unwrap();

        let after = ops(&conn);
        assert_eq!(
            &after[before as usize..],
            ["delete", "delete", "insert"],
            "two copies cleared, the envelope's one written back"
        );
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    /// `OnExisting::Fail` asks "would this import destroy something". A
    /// collection someone emptied has an outbox full of the deletes that
    /// emptied it — which is not something to protect, and must not be read
    /// as a non-empty collection.
    #[test]
    fn outbox_rows_alone_do_not_make_a_collection_look_occupied() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        collection::delete(&conn, id).unwrap();
        assert_eq!(count(&conn), 2, "the add and the delete");

        let empty = {
            let (_d, c) = user_conn();
            crate::json_backup::export(&c).unwrap()
        };
        crate::json_backup::import(&mut conn, &empty, crate::json_backup::OnExisting::Fail)
            .expect("an emptied collection is still an empty collection");
    }

    #[test]
    fn an_envelope_that_names_the_outbox_is_ignored_rather_than_restored() {
        let (_dir, mut conn) = user_conn();
        let mut envelope: serde_json::Value =
            serde_json::from_str(&crate::json_backup::export(&conn).unwrap()).unwrap();
        // What a build from before pd-5m54 would have written: the exporter
        // walked every table, so its envelopes carry the outbox.
        envelope.as_object_mut().unwrap().insert(
            TABLE.to_string(),
            serde_json::json!([{
                "seq": 1,
                "occurred_at": "2026-01-01T00:00:00.000Z",
                "source_table": "collection",
                "op": "insert",
                "row_id": 99,
                "payload": "{}"
            }]),
        );

        crate::json_backup::import(
            &mut conn,
            &serde_json::to_string(&envelope).unwrap(),
            crate::json_backup::OnExisting::Replace,
        )
        .expect("an old envelope still restores");

        assert_eq!(
            count(&conn),
            0,
            "the stale event was dropped, not replayed into a fresh database"
        );
    }

    // =================================================================
    // Emitting current state — backfill, redrive, DR reconcile (pd-385w)
    // =================================================================

    /// The holdings the outbox describes, resolved by the rule the tenant
    /// zone resolves by. Every proof below is stated against this rather
    /// than against the events, because the events are transport and the
    /// projection is what a valuation is computed from.
    fn projection(conn: &Connection) -> BTreeMap<(String, i64), serde_json::Value> {
        project(&events(conn).unwrap())
    }

    /// A collection with some history in it: two copies acquired, one
    /// edited, one moved into a binder, one sold, one deleted outright.
    /// Enough that a projection is not trivially the insert list.
    fn a_collection_with_history(conn: &mut Connection) -> (i64, i64) {
        let kept = collection::add(conn, &new_copy("sv3pt5-1-normal")).unwrap();
        let sold = collection::add(conn, &new_copy("sv3pt5-1-holo")).unwrap();
        let gone = collection::add(conn, &new_copy("sv3pt5-2-normal")).unwrap();

        collection::update(
            conn,
            kept,
            &collection::CopyEdit {
                condition: Some("Lightly Played".into()),
                ..Default::default()
            },
        )
        .unwrap();
        collection::set_status(conn, sold, "sold", None).unwrap();
        collection::delete(conn, gone).unwrap();
        (kept, sold)
    }

    fn key(id: i64) -> (String, i64) {
        ("collection".to_string(), id)
    }

    // -----------------------------------------------------------------
    // The headline proof
    // -----------------------------------------------------------------

    /// **Delete the tenant zone entirely and rebuild it by backfill; the
    /// result equals what incremental shipping produced.**
    ///
    /// The row-identical discipline of the lake-as-source design, applied
    /// to the inbound leg. It is the only test that shows backfill and the
    /// everyday path genuinely *agree* rather than that both run: the
    /// left-hand side is the projection of events the triggers wrote one
    /// mutation at a time, the right-hand side is the projection of events
    /// [`emit`] wrote in one pass over the finished table.
    ///
    /// Throwing the events away is what "delete the zone" means here — the
    /// zone holds exactly this projection, so a zone with nothing in it and
    /// an outbox with nothing in it are the same starting point.
    #[test]
    fn a_zone_rebuilt_by_backfill_equals_the_zone_incremental_shipping_built() {
        let (_dir, mut conn) = user_conn();
        a_collection_with_history(&mut conn);

        let incremental = projection(&conn);
        assert_eq!(incremental.len(), 2, "one deleted, two survive");

        conn.execute(&format!("DELETE FROM {TABLE}"), []).unwrap();
        assert!(projection(&conn).is_empty(), "the zone is gone");

        let run = emit(&mut conn, &Scope::Collection, false).unwrap();
        assert_eq!(run.source, Source::Backfill);
        assert_eq!(run.events, 2);

        assert_eq!(
            projection(&conn),
            incremental,
            "the rebuilt zone holds what incremental shipping produced"
        );
    }

    /// The same claim with the history left in place — a backfill run over
    /// a collection whose events were never lost. The emitted events tie
    /// with the ones already there and carry the same payloads, so the
    /// projection cannot move. A backfill is not something to be afraid of
    /// running.
    #[test]
    fn a_backfill_over_an_intact_outbox_changes_nothing() {
        let (_dir, mut conn) = user_conn();
        a_collection_with_history(&mut conn);
        let before = projection(&conn);

        emit(&mut conn, &Scope::Collection, false).unwrap();

        assert_eq!(projection(&conn), before);
    }

    // -----------------------------------------------------------------
    // Rule 3 — last-write-wins by occurred_at, in the failing direction
    // -----------------------------------------------------------------

    /// **A redrive of stale state must not clobber a newer live mutation.**
    ///
    /// Constructed directly, because the shape that breaks is the one a
    /// redrive built from a snapshot taken *before* a live write produces:
    /// an event carrying old content, dated at the old instant, appended
    /// with a `seq` higher than the live event's.
    ///
    /// Seen red: ordering [`project`] by `seq` alone leaves this asserting
    /// `Damaged` and finding `Near Mint` — the redrive's stale condition
    /// silently overwriting the tenant's real one.
    #[test]
    fn a_stale_redrive_with_a_higher_seq_does_not_clobber_a_live_mutation() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();

        // The state a redrive's snapshot would have caught.
        let stale = evs(&conn)[0].clone();
        assert_eq!(stale.seq, 1);

        // ...and then a live mutation, shipped normally.
        collection::update(
            &conn,
            id,
            &collection::CopyEdit {
                condition: Some("Damaged".into()),
                ..Default::default()
            },
        )
        .unwrap();

        // The redrive lands afterwards, with a NEW and higher seq, carrying
        // the row as it was when the snapshot was taken.
        conn.execute(
            &format!(
                "INSERT INTO {TABLE} \
                     (occurred_at, source_table, op, row_id, payload, source) \
                 VALUES (?1, 'collection', 'insert', ?2, ?3, 'redrive')"
            ),
            rusqlite::params![stale.occurred_at, id, stale.payload],
        )
        .unwrap();
        let last = evs(&conn).last().unwrap().clone();
        assert!(last.seq > 2, "the redrive really is the highest seq");

        assert_eq!(
            projection(&conn)[&key(id)]["condition"],
            "Damaged",
            "the live mutation survives — resolution is by occurred_at, and \
             the redrive carries the row's own timestamp, not the moment it \
             was re-emitted"
        );
    }

    /// The property the test above depends on: [`emit`] dates an event with
    /// the row's own last-known change time. If it stamped `now`, a redrive
    /// would win every race by construction and rule 3 would be words.
    #[test]
    fn an_emitted_event_carries_the_rows_own_time_not_the_moment_of_emission() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        collection::set_status(&mut conn, id, "listed", None).unwrap();
        let newest = evs(&conn).last().unwrap().occurred_at.clone();

        emit(&mut conn, &Scope::Row(id), false).unwrap();

        let emitted = evs(&conn).last().unwrap().clone();
        assert_eq!(emitted.source, "redrive");
        assert_eq!(
            emitted.occurred_at, newest,
            "the row last changed then, and that is what the event says"
        );
    }

    /// A row that predates the triggers has no event to take a time from,
    /// so it takes its own — normalised into the outbox's format, because
    /// `occurred_at` is the column the whole resolution rule sorts on and
    /// two formats in it do not order against each other.
    #[test]
    fn a_row_that_never_emitted_is_dated_from_its_own_timestamp() {
        let (_dir, mut conn) = user_conn();
        collection::add(
            &mut conn,
            &collection::NewCopy {
                acquired_at: Some("2020-01-02T03:04:05+00:00".into()),
                ..new_copy("sv3pt5-1-normal")
            },
        )
        .unwrap();
        // What a collection populated before the triggers existed looks
        // like: rows, and no events describing them.
        conn.execute(&format!("DELETE FROM {TABLE}"), []).unwrap();

        emit(&mut conn, &Scope::Collection, false).unwrap();

        assert_eq!(evs(&conn)[0].occurred_at, "2020-01-02T03:04:05.000Z");
    }

    /// ...and a timestamp SQLite cannot read falls to the floor rather than
    /// putting an unorderable string in the sort column. An event dated
    /// there loses to everything, which is the safe reading of "we do not
    /// know when this row last changed".
    #[test]
    fn a_row_whose_own_timestamp_is_unreadable_is_dated_to_the_floor() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        conn.execute("UPDATE collection SET acquired_at = 'sometime in 2019'", [])
            .unwrap();
        conn.execute(&format!("DELETE FROM {TABLE}"), []).unwrap();

        emit(&mut conn, &Scope::Collection, false).unwrap();

        assert_eq!(evs(&conn)[0].occurred_at, UNDATED);
    }

    // -----------------------------------------------------------------
    // Rule 1 — through the outbox, and rule 2 — provenance only
    // -----------------------------------------------------------------

    /// An emitted payload is byte-identical to the one the trigger wrote
    /// for the same unchanged row. Both are built from the table's declared
    /// columns in declaration order — the trigger by hand in
    /// `schema_user.sql`, the emitter from `pragma_table_info` — so this is
    /// the assertion that the two lists have not drifted.
    #[test]
    fn an_emitted_payload_is_byte_identical_to_the_triggers() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        let from_trigger = evs(&conn)[0].payload.clone();

        emit(&mut conn, &Scope::Row(id), false).unwrap();

        assert_eq!(evs(&conn)[1].payload, from_trigger);
    }

    /// Rule 2. The column exists, it is honest about all three writers, and
    /// nothing about the event is otherwise different — same table, same
    /// op, same payload shape. A consumer that branches on it is the thing
    /// this column must never cause.
    #[test]
    fn provenance_is_recorded_and_is_the_only_difference() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        emit(&mut conn, &Scope::Row(id), false).unwrap();
        emit(&mut conn, &Scope::Collection, false).unwrap();

        let evs = evs(&conn);
        let sources: Vec<&str> = evs.iter().map(|e| e.source.as_str()).collect();
        assert_eq!(sources, ["trigger", "redrive", "backfill"]);

        for e in &evs {
            assert_eq!(e.source_table, "collection");
            assert_eq!(e.op, "insert");
            assert_eq!(e.payload, evs[0].payload);
        }
    }

    /// Rule 1. `emit` appends outbox rows and records the run; it writes no
    /// holding. A path that reached the zone directly would be a second
    /// writer, and the zone could then disagree with the outbox with
    /// nothing able to detect it.
    #[test]
    fn emitting_changes_no_holding() {
        let (_dir, mut conn) = user_conn();
        a_collection_with_history(&mut conn);
        let before: Vec<String> = collection::list(&conn, 1000, 0)
            .unwrap()
            .into_iter()
            .map(|c| format!("{}:{}:{}", c.id, c.printing_id, c.status))
            .collect();

        emit(&mut conn, &Scope::Collection, false).unwrap();

        let after: Vec<String> = collection::list(&conn, 1000, 0)
            .unwrap()
            .into_iter()
            .map(|c| format!("{}:{}:{}", c.id, c.printing_id, c.status))
            .collect();
        assert_eq!(after, before);
    }

    /// **The `ALTER` path, which no fresh database exercises.** A collection
    /// created between pd-5m54 and pd-385w already carries
    /// `ownership_outbox`, so `CREATE TABLE IF NOT EXISTS` does nothing to it
    /// and the provenance column arrives only through
    /// `connection::USER_ADDED_COLUMNS`. Every other test here builds a
    /// database that never needed it — which is exactly how a convergence
    /// step ships broken and is only found on the one box that had the old
    /// shape.
    ///
    /// It also pins the thing the `DEFAULT` is doing: events written by the
    /// triggers a pre-pd-385w collection still carries are labelled
    /// `trigger`, because that is what they are, without those triggers
    /// having to be replaced.
    #[test]
    fn a_collection_from_before_the_provenance_column_grows_one_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");

        // The pre-pd-385w shape: the outbox as pd-5m54 declared it, with an
        // event already in it, and the triggers that wrote it.
        {
            let conn = crate::open_user(&path).unwrap();
            conn.execute_batch(&format!(
                "DROP TABLE {TABLE}; \
                 CREATE TABLE {TABLE} ( \
                     seq INTEGER PRIMARY KEY AUTOINCREMENT, \
                     occurred_at TEXT NOT NULL, source_table TEXT NOT NULL, \
                     op TEXT NOT NULL CHECK (op IN ('insert','update','delete')), \
                     row_id INTEGER NOT NULL, payload TEXT NOT NULL);"
            ))
            .unwrap();
            conn.execute(
                "INSERT INTO collection (printing_id, acquired_at, source) \
                 VALUES ('sv3pt5-1-normal', '2024-01-01T00:00:00Z', 'manual_id')",
                [],
            )
            .unwrap();
        }
        let has_source = |c: &Connection| {
            c.prepare(&format!(
                "SELECT 1 FROM pragma_table_info('{TABLE}') WHERE name = 'source'"
            ))
            .unwrap()
            .exists([])
            .unwrap()
        };
        assert!(
            !has_source(&rusqlite::Connection::open(&path).unwrap()),
            "the fixture really is the old shape"
        );

        let mut conn = crate::open_user(&path).unwrap();
        assert!(has_source(&conn), "...and opening it converged the shape");

        let old = &evs(&conn)[0];
        assert_eq!(
            old.source, "trigger",
            "the event the old triggers wrote is labelled for what it is"
        );

        // ...and the column is a working one, not just present.
        emit(&mut conn, &Scope::Collection, false).unwrap();
        assert_eq!(evs(&conn)[1].source, "backfill");
        let refused = conn.execute(
            &format!(
                "INSERT INTO {TABLE} \
                     (occurred_at, source_table, op, row_id, payload, source) \
                 VALUES ('2024-01-01T00:00:00.000Z', 'collection', 'insert', 1, '{{}}', 'nonsense')"
            ),
            [],
        );
        assert!(refused.is_err(), "the CHECK came with the column");
    }

    /// The list the emitter works from is checked against the triggers
    /// rather than trusted. `sealed_collection` is a holding like any other
    /// (pd-4gop) and its triggers are a separate change; the day they land,
    /// this fails until [`SOURCE_TABLES`] grows the entry.
    ///
    /// Without it, a backfill would cover singles, report success, and
    /// leave every tenant's sealed product invisible to the zone — the same
    /// silent under-report the whole item exists to prevent, one layer in.
    #[test]
    fn every_triggered_table_is_emittable() {
        let (_dir, conn) = user_conn();
        let mut triggered: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT tbl_name FROM sqlite_master \
                     WHERE type = 'trigger' AND name LIKE '%\\_outbox\\_%' ESCAPE '\\'",
                )
                .unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        triggered.sort();
        assert!(!triggered.is_empty(), "the outbox triggers still exist");

        let mut emittable: Vec<String> = SOURCE_TABLES
            .iter()
            .map(|(t, _)| (*t).to_string())
            .collect();
        emittable.sort();

        assert_eq!(
            triggered, emittable,
            "\n\nEvery table with outbox triggers must be backfillable. A table \
             the triggers cover and the emitter does not is a holding class that \
             silently never reaches the tenant zone — backfilled singles, missing \
             sealed, and nothing anywhere saying so.\n\n\
             If you have just ADDED triggers (pd-4gop, sealed_collection), the fix \
             is one line in outbox::SOURCE_TABLES:\n\n    \
             (\"<table>\", \"<its own timestamp column>\")\n\n\
             The second element dates a row that has never emitted an event — it \
             must be something SQLite's strftime can parse. Nothing else needs \
             touching: the payload is built from pragma_table_info, so it matches \
             whatever your triggers declare.\n\n\
             If you have just REMOVED triggers, drop the entry.\n"
        );
    }

    /// Every table the emitter names must actually declare the column it
    /// dates undated rows from. A typo here would not fail loudly: the
    /// `strftime` would error at emit time, on the rare path, at 3am.
    #[test]
    fn every_emittable_table_declares_its_timestamp_column() {
        let (_dir, conn) = user_conn();
        for (table, ts_col) in SOURCE_TABLES {
            let present: bool = conn
                .prepare(&format!(
                    "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
                ))
                .unwrap()
                .exists([ts_col])
                .unwrap();
            assert!(present, "{table} declares no column {ts_col}");
        }
    }

    // -----------------------------------------------------------------
    // Rule 4 — safe, but not silent
    // -----------------------------------------------------------------

    #[test]
    fn a_second_backfill_refuses_and_says_when_the_first_was() {
        let (_dir, mut conn) = user_conn();
        a_collection_with_history(&mut conn);
        emit(&mut conn, &Scope::Collection, false).unwrap();
        let first = last_backfill(&conn).unwrap().unwrap();

        let err = emit(&mut conn, &Scope::Collection, false).unwrap_err();
        let DbError::Conflict(message) = &err else {
            panic!("{err:?}");
        };
        assert!(
            message.contains(&first.completed_at),
            "the refusal names when the first ran: {message}"
        );
        assert!(message.contains("--force"), "...and what to do: {message}");
    }

    #[test]
    fn a_forced_second_backfill_runs_and_changes_nothing() {
        let (_dir, mut conn) = user_conn();
        a_collection_with_history(&mut conn);
        emit(&mut conn, &Scope::Collection, false).unwrap();
        let after_first = projection(&conn);

        let second = emit(&mut conn, &Scope::Collection, true).unwrap();
        assert_eq!(second.events, 2);

        assert_eq!(
            projection(&conn),
            after_first,
            "idempotent — the payload is a whole row, so replaying it is an \
             upsert to the same value"
        );
        assert!(runs(&conn).unwrap()[0].forced);
    }

    /// A redrive is targeted and expected to repeat — the ledger records
    /// it, and nothing refuses it. Only the full backfill is the thing an
    /// operator can run twice by mistake.
    #[test]
    fn a_redrive_is_recorded_but_never_refused() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        emit(&mut conn, &Scope::Row(id), false).unwrap();
        emit(&mut conn, &Scope::Row(id), false).unwrap();

        let log = runs(&conn).unwrap();
        assert_eq!(log.len(), 2);
        assert!(log.iter().all(|r| r.source == "redrive"));
        assert_eq!(log[0].scope, format!("row:{id}"));
    }

    #[test]
    fn the_ledger_records_the_seq_range_the_run_appended() {
        let (_dir, mut conn) = user_conn();
        a_collection_with_history(&mut conn);
        let before: i64 = conn
            .query_row(&format!("SELECT max(seq) FROM {TABLE}"), [], |r| r.get(0))
            .unwrap();

        let run = emit(&mut conn, &Scope::Collection, false).unwrap();

        assert_eq!(run.seq_first, Some(before + 1));
        assert_eq!(run.seq_last, Some(before + run.events as i64));
        let logged = &runs(&conn).unwrap()[0];
        assert_eq!(logged.seq_first, run.seq_first);
        assert_eq!(logged.seq_last, run.seq_last);
        assert_eq!(logged.rows_emitted, run.events as i64);
    }

    /// The range a run reports must be the numbers it actually wrote, on an
    /// outbox whose shipped prefix has been trimmed — which is the normal
    /// state of a live one, and the state every real backfill runs against.
    ///
    /// Found by running the command rather than by reading it: a collection
    /// seeded and then trimmed reported `seq 1..6` for three events sitting
    /// at 4..6. `seq` is AUTOINCREMENT so that a trimmed number is never
    /// reused, which means the highest number PRESENT and the last number
    /// ALLOCATED are different values — and a redrive aimed at the range
    /// this reports would have named events that do not exist.
    #[test]
    fn the_reported_range_is_the_numbers_written_not_the_rows_present() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        // The shipper shipped those two and trimmed them.
        conn.execute(&format!("DELETE FROM {TABLE}"), []).unwrap();

        let run = emit(&mut conn, &Scope::Collection, false).unwrap();

        assert_eq!(run.events, 2);
        assert_eq!((run.seq_first, run.seq_last), (Some(3), Some(4)));
        let written: Vec<i64> = evs(&conn).into_iter().map(|e| e.seq).collect();
        assert_eq!(written, [3, 4], "and those are the numbers on disk");
    }

    /// A backfill of an empty collection is a real run that emitted
    /// nothing, not a no-op to hide. It takes a ledger row — so the next
    /// one still refuses — and no seq range, because it appended none.
    #[test]
    fn a_backfill_of_an_empty_collection_is_recorded_with_no_range() {
        let (_dir, mut conn) = user_conn();

        let run = emit(&mut conn, &Scope::Collection, false).unwrap();
        assert_eq!(run.events, 0);
        assert_eq!((run.seq_first, run.seq_last), (None, None));

        assert!(emit(&mut conn, &Scope::Collection, false).is_err());
    }

    // -----------------------------------------------------------------
    // Redrive — the scopes
    // -----------------------------------------------------------------

    #[test]
    fn a_seq_range_redrives_exactly_the_rows_that_range_named() {
        let (_dir, mut conn) = user_conn();
        let a = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        let b = collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        collection::add(&mut conn, &new_copy("sv3pt5-2-normal")).unwrap();

        // seq 1 named a, seq 2 named b; seq 3's row is outside the range.
        let run = emit(&mut conn, &Scope::Seq { from: 1, to: 2 }, false).unwrap();
        assert_eq!(run.events, 2);
        assert_eq!(run.scope, "seq:1..2");

        let redriven: Vec<i64> = evs(&conn)
            .into_iter()
            .filter(|e| e.source == "redrive")
            .map(|e| e.row_id)
            .collect();
        assert_eq!(redriven, [a, b]);
    }

    /// A redrive exists because a slice of the stream was lost. If that
    /// slice removed a holding, replaying only the survivors would leave
    /// the zone holding a card the tenant does not own — so a row the scope
    /// names that is gone re-emits its last-known `delete`.
    #[test]
    fn a_redrive_reproduces_the_removals_in_the_slice_it_covers() {
        let (_dir, mut conn) = user_conn();
        let kept = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        let gone = collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        collection::delete(&conn, gone).unwrap();
        let last = evs(&conn).last().unwrap().seq;

        let run = emit(&mut conn, &Scope::Seq { from: 1, to: last }, false).unwrap();
        assert_eq!(run.events, 2, "one survivor re-emitted, one removal");

        let fresh: Vec<(String, i64)> = evs(&conn)
            .into_iter()
            .filter(|e| e.source == "redrive")
            .map(|e| (e.op, e.row_id))
            .collect();
        assert_eq!(
            fresh,
            [("insert".to_string(), kept), ("delete".to_string(), gone)]
        );
    }

    /// ...and a full backfill deliberately does not. Its job is "these are
    /// the rows", against a zone being rebuilt from nothing; re-emitting
    /// every historical removal would grow the outbox with events the
    /// rebuilt zone has no use for.
    #[test]
    fn a_backfill_does_not_reemit_historical_removals() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        let gone = collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        collection::delete(&conn, gone).unwrap();

        let run = emit(&mut conn, &Scope::Collection, false).unwrap();
        assert_eq!(run.events, 1);
        assert!(
            evs(&conn)
                .iter()
                .all(|e| e.source != "backfill" || e.op == "insert")
        );
    }

    #[test]
    fn a_scope_that_names_no_row_emits_nothing_and_is_still_recorded() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();

        let run = emit(&mut conn, &Scope::Row(9999), false).unwrap();
        assert_eq!(run.events, 0);
        assert_eq!(runs(&conn).unwrap().len(), 1);
    }

    #[test]
    fn a_backwards_seq_range_is_refused() {
        let (_dir, mut conn) = user_conn();
        let err = emit(&mut conn, &Scope::Seq { from: 9, to: 2 }, false).unwrap_err();
        assert!(matches!(err, DbError::Invalid(_)), "{err:?}");
        assert!(runs(&conn).unwrap().is_empty(), "a refusal is not a run");
    }

    #[test]
    fn a_seq_range_starting_below_one_is_refused() {
        let (_dir, mut conn) = user_conn();
        let err = emit(&mut conn, &Scope::Seq { from: 0, to: 5 }, false).unwrap_err();
        assert!(matches!(err, DbError::Invalid(_)), "{err:?}");
    }

    // -----------------------------------------------------------------
    // DR reconcile
    // -----------------------------------------------------------------

    /// A tenant restored from a Litestream replica to an earlier point,
    /// with a zone that lost the events describing it. The redrive
    /// reconciles the zone to the restored state — which is the third use
    /// of the one operation, and needs no code of its own.
    ///
    /// The restore is modelled the way a physical one behaves: the file
    /// comes back holding both the rows and the outbox as they were at the
    /// restore point, and the events written after it are simply not there.
    #[test]
    fn a_redrive_reconciles_the_zone_to_a_database_restored_to_an_earlier_point() {
        let (_dir, mut conn) = user_conn();
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        collection::update(
            &conn,
            id,
            &collection::CopyEdit {
                condition: Some("Lightly Played".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let restore_point = projection(&conn);
        let restored_to = evs(&conn).last().unwrap().seq;

        // ...the collection carried on, and then the file was rolled back.
        collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        collection::delete(&conn, id).unwrap();
        conn.execute(
            &format!("DELETE FROM {TABLE} WHERE seq > ?1"),
            [restored_to],
        )
        .unwrap();
        conn.execute("DELETE FROM collection", []).unwrap();
        conn.execute(
            &format!("DELETE FROM {TABLE} WHERE seq > ?1"),
            [restored_to],
        )
        .unwrap();
        for (k, v) in &restore_point {
            let cols: Vec<&String> = v.as_object().unwrap().keys().collect();
            let placeholders = (1..=cols.len())
                .map(|i| format!("?{i}"))
                .collect::<Vec<_>>()
                .join(", ");
            let names = cols
                .iter()
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let values: Vec<rusqlite::types::Value> =
                cols.iter().map(|c| json_to_sql(&v[c.as_str()])).collect();
            conn.execute(
                &format!("INSERT INTO collection ({names}) VALUES ({placeholders})"),
                rusqlite::params_from_iter(values),
            )
            .unwrap();
            assert_eq!(k.0, "collection");
        }
        conn.execute(
            &format!("DELETE FROM {TABLE} WHERE seq > ?1"),
            [restored_to],
        )
        .unwrap();

        // The zone was lost with the incident. Rebuild it from the
        // restored database — the same command, a different scope.
        conn.execute(&format!("DELETE FROM {TABLE}"), []).unwrap();
        emit(&mut conn, &Scope::Collection, false).unwrap();

        assert_eq!(
            projection(&conn),
            restore_point,
            "the zone now says exactly what the restored database says"
        );
    }

    fn json_to_sql(v: &serde_json::Value) -> rusqlite::types::Value {
        use rusqlite::types::Value;
        match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Bool(b) => Value::Integer(*b as i64),
            serde_json::Value::Number(n) if n.is_i64() => Value::Integer(n.as_i64().unwrap()),
            serde_json::Value::Number(n) => Value::Real(n.as_f64().unwrap()),
            other => Value::Text(other.as_str().unwrap_or_default().to_string()),
        }
    }

    // -----------------------------------------------------------------
    // Not collection state, part two
    // -----------------------------------------------------------------

    #[test]
    fn the_emit_ledger_is_not_carried_by_the_json_envelope() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        emit(&mut conn, &Scope::Collection, false).unwrap();

        let envelope: serde_json::Value =
            serde_json::from_str(&crate::json_backup::export(&conn).unwrap()).unwrap();
        assert!(
            envelope.get(EMIT_LOG).is_none(),
            "a restored database has not been backfilled just because the \
             database the envelope came from had"
        );
    }
}
