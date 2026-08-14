//! The ownership outbox — the record of holdings changes leaving a
//! collection, written atomically with the change that caused it (pd-5m54,
//! extended to sealed product by pd-4gop).
//!
//! There is deliberately almost no code here. The table and its writers are
//! declared in `schema_user.sql`: three `AFTER INSERT/UPDATE/DELETE`
//! triggers per source table, each appending one row. A trigger fires inside
//! the statement's own transaction, so a holding cannot change without the
//! event being recorded and an event cannot survive a mutation that rolled
//! back — atomicity by construction, with no instant in between for a crash
//! to land in and no call site that has to remember to write.
//!
//! This module holds the two names the rest of the crate needs ([`TABLE`]
//! and [`SOURCES`]) and the tests that hold the contract in place. It does
//! not read the outbox: the consumer is the shipper, which is its own change
//! (item 4 of the inbound-leg epic, pd-dxn3).
//!
//! ## What the contract is
//!
//! * Every mutation of a [`SOURCES`] table appends exactly one row, in the
//!   same transaction as the mutation.
//! * `seq` is monotonic, gap-free and never reused — a missing number means
//!   an event was LOST, which is what makes the offline side's consistency
//!   provable rather than assumed. It is ONE sequence across all sources, so
//!   it orders a single's change against a sealed one.
//! * `payload` is the whole row as a JSON object — the post-image for
//!   `insert`/`update`, the pre-image for `delete`.
//! * `row_id` is unique only WITHIN a `source_table`. A consumer projects on
//!   the `(source_table, row_id)` PAIR: the two tables number their rows
//!   independently, so replaying on `row_id` alone merges holdings that
//!   merely share a number.
//!
//! ## What it is not
//!
//! It is not the tenant's collection state, and so it is not part of the
//! portable JSON envelope (`crate::json_backup`). It is the log of changes
//! *leaving* the collection: an envelope that carried it would restore
//! already-shipped events into a fresh database, and a restore already
//! records itself here — its deletes and its inserts fire the triggers like
//! any other write.

/// The outbox table's name. The one place it is spelled in Rust.
pub const TABLE: &str = "ownership_outbox";

/// The holdings tables the outbox records, spelled exactly as their events
/// carry them in `source_table`.
///
/// Both halves of a tenant's holdings: singles (`collection`, one row per
/// physical card) and sealed product (`sealed_collection`, one row per lot,
/// carrying a `quantity`). A valuation built from the first alone
/// under-reports, which is the wrong direction to be wrong in — pd-4gop.
///
/// A *claim about* the schema rather than a second description of it:
/// `every_outbox_source_is_declared_here` compares this list against the
/// triggers `sqlite_master` actually carries, both directions, so a third
/// source with triggers and no entry here — or an entry here whose triggers
/// were never written — fails the build.
pub const SOURCES: [&str; 2] = ["collection", "sealed_collection"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbError, collection, connect_user, sealed};
    use rusqlite::Connection;
    use std::collections::BTreeSet;

    /// A user connection whose catalog holds two printings of one card and
    /// one printing of another — enough to exercise `change_printing` in
    /// both the allowed and the refused direction — plus two sealed
    /// products, because `sealed::add` validates against the catalog.
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
            for (product, name) in [
                (5001, "151 Elite Trainer Box"),
                (5002, "151 Booster Bundle"),
            ] {
                c.execute(
                    "INSERT INTO sealed_products (product_id, name, category, fetched_at) \
                     VALUES (?1, ?2, 'elite_trainer_box', '2026-05-18')",
                    rusqlite::params![product, name],
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

    fn new_sealed(product_id: i64) -> sealed::NewSealed {
        sealed::NewSealed {
            product_id,
            source: Some("test".to_string()),
            ..Default::default()
        }
    }

    /// An outbox row, as the tests read it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Event {
        seq: i64,
        source: String,
        op: String,
        row_id: i64,
        payload: String,
    }

    impl Event {
        fn payload(&self) -> serde_json::Value {
            serde_json::from_str(&self.payload).unwrap()
        }
    }

    /// Every outbox row, in sequence order.
    fn events(conn: &Connection) -> Vec<Event> {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT seq, source_table, op, row_id, payload FROM {TABLE} ORDER BY seq"
            ))
            .unwrap();
        let rows = stmt
            .query_map([], |r| {
                Ok(Event {
                    seq: r.get(0)?,
                    source: r.get(1)?,
                    op: r.get(2)?,
                    row_id: r.get(3)?,
                    payload: r.get(4)?,
                })
            })
            .unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    /// Every outbox row from one source table, in sequence order.
    fn events_from(conn: &Connection, source: &str) -> Vec<Event> {
        events(conn)
            .into_iter()
            .filter(|e| e.source == source)
            .collect()
    }

    fn ops(conn: &Connection) -> Vec<String> {
        events(conn).into_iter().map(|e| e.op).collect()
    }

    /// `(source_table, op)` for every row, in sequence order — the shape the
    /// interleaving tests assert.
    fn source_ops(conn: &Connection) -> Vec<(String, String)> {
        events(conn).into_iter().map(|e| (e.source, e.op)).collect()
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

        let evs = events(&conn);
        assert_eq!(evs.len(), 1, "one mutation, one event");
        assert_eq!(evs[0].seq, 1);
        assert_eq!(evs[0].source, "collection");
        assert_eq!(evs[0].op, "insert");
        assert_eq!(evs[0].row_id, id);

        let payload = evs[0].payload();
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

        let evs = events(&conn);
        assert_eq!(ops(&conn), ["insert", "delete"]);
        assert_eq!(
            evs[1].payload()["printing_id"],
            "sv3pt5-1-holo",
            "a delete records the pre-image — the row is gone, so nothing \
             else can say what was lost"
        );
        assert_eq!(evs[1].row_id, id);
    }

    /// The columns of `table`, in declaration order.
    fn declared_columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(1)).unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    /// The gate that stops a column being added to a holdings table and
    /// quietly missing from the event. The payload is a hand-written
    /// `json_object` in every trigger — SQLite has no `NEW.*` — so nothing
    /// but this comparison keeps the lists honest.
    ///
    /// Run per source and per op, so a column added to one trigger and not
    /// its two siblings is caught, and so is a column added to `collection`'s
    /// three and forgotten in `sealed_collection`'s.
    ///
    /// Seen red: dropping `'grade_cert', NEW.grade_cert` from the collection
    /// insert trigger fails it with `payload is missing: ["grade_cert"]`, and
    /// dropping `'quantity', NEW.quantity` from the sealed insert trigger
    /// fails it with `payload is missing: ["quantity"]`.
    #[test]
    fn every_column_of_every_source_reaches_the_payload() {
        let (_dir, mut conn) = user_conn();

        // Every op on every source, in one database — which also puts the
        // two sources' events in one sequence, as production will.
        let id = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        collection::set_status(&mut conn, id, "sold", None).unwrap();
        collection::delete(&conn, id).unwrap();

        let sealed_id = sealed::add(&conn, &new_sealed(5001)).unwrap();
        sealed::update(
            &conn,
            sealed_id,
            &sealed::SealedEdit {
                status: Some("opened".into()),
                ..Default::default()
            },
        )
        .unwrap();
        sealed::delete(&conn, sealed_id).unwrap();

        for source in SOURCES {
            let declared = declared_columns(&conn, source);
            let evs = events_from(&conn, source);
            assert_eq!(evs.len(), 3, "{source}: one event per op");
            for ev in &evs {
                let payload = ev.payload();
                let carried: Vec<&String> = payload.as_object().unwrap().keys().collect();
                let missing: Vec<&String> =
                    declared.iter().filter(|c| !carried.contains(c)).collect();
                assert!(
                    missing.is_empty(),
                    "{source} {} event (seq {}) payload is missing: {missing:?}",
                    ev.op,
                    ev.seq
                );
                let extra: Vec<&&String> = carried
                    .iter()
                    .filter(|k| !declared.iter().any(|c| c == **k))
                    .collect();
                assert!(
                    extra.is_empty(),
                    "{source} {} event (seq {}) payload invents: {extra:?}",
                    ev.op,
                    ev.seq
                );
            }
        }
    }

    /// [`SOURCES`] is a claim about the schema, and this is what makes it
    /// one. Every table `schema_user.sql` hangs outbox triggers on must be
    /// named there, and every name there must have triggers — a third source
    /// wired up and not declared would leave the shipper reading a
    /// `source_table` it has never heard of, and a name declared without
    /// triggers is a source silently shipping nothing.
    ///
    /// The trigger names are read off `sqlite_master` rather than listed, so
    /// this cannot pass by agreeing with a copy of itself.
    #[test]
    fn every_outbox_source_is_declared_here() {
        let (_dir, conn) = user_conn();

        let mut stmt = conn
            .prepare(
                "SELECT tbl_name FROM sqlite_master \
                 WHERE type = 'trigger' AND sql LIKE '%INSERT INTO ' || ?1 || '%'",
            )
            .unwrap();
        let wired: BTreeSet<String> = stmt
            .query_map([TABLE], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        let declared: BTreeSet<String> = SOURCES.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            wired, declared,
            "the tables carrying outbox triggers and outbox::SOURCES disagree"
        );
    }

    /// Three per source — insert, update, delete. A source wired up with two
    /// of them has a hole that only shows as a missing event much later.
    #[test]
    fn every_source_carries_all_three_triggers() {
        let (_dir, conn) = user_conn();
        for source in SOURCES {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master \
                     WHERE type = 'trigger' AND tbl_name = ?1 \
                       AND sql LIKE '%INSERT INTO ' || ?2 || '%'",
                    rusqlite::params![source, TABLE],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 3, "{source} should have insert/update/delete triggers");
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

    /// The same claim for the other half of the holdings (pd-4gop). Every
    /// function in `sealed.rs` that writes the table, and none of them
    /// mentions the outbox either.
    #[test]
    fn every_sealed_mutation_appends_an_event() {
        let (_dir, conn) = user_conn();

        let id = sealed::add(&conn, &new_sealed(5001)).unwrap();
        assert_eq!(count(&conn), 1, "add");

        assert!(
            sealed::update(
                &conn,
                id,
                &sealed::SealedEdit {
                    quantity: Some(3),
                    ..Default::default()
                }
            )
            .unwrap()
        );
        assert_eq!(count(&conn), 2, "update");

        // Disposal is an update of `status`, not a delete — the lot is still
        // a holding, it is just no longer owned, and the offline side has to
        // see that rather than infer it from an absence.
        assert!(
            sealed::update(
                &conn,
                id,
                &sealed::SealedEdit {
                    status: Some("sold".into()),
                    sale_price: Some(120.0),
                    ..Default::default()
                }
            )
            .unwrap()
        );
        assert_eq!(count(&conn), 3, "dispose");

        assert!(sealed::delete(&conn, id).unwrap());
        assert_eq!(count(&conn), 4, "delete");

        let evs = events_from(&conn, "sealed_collection");
        assert_eq!(
            evs.iter().map(|e| e.op.as_str()).collect::<Vec<_>>(),
            ["insert", "update", "update", "delete"]
        );
        assert!(
            evs.iter().all(|e| e.row_id == id),
            "every event names the lot it describes"
        );

        // The whole row, quantity and all — a lot of three boxes is one
        // event carrying `quantity: 3`, never three events.
        let sold = evs[2].payload();
        assert_eq!(sold["quantity"], 3);
        assert_eq!(sold["status"], "sold");
        assert_eq!(sold["sale_price"], 120.0);
        assert_eq!(sold["product_id"], 5001);

        // ...and the delete carries the lot as it was.
        let gone = evs[3].payload();
        assert_eq!(gone["quantity"], 3);
        assert_eq!(gone["status"], "sold");
    }

    /// `row_id` is `<source_table>.id`, and the two tables number their rows
    /// independently — so the FIRST single and the FIRST sealed lot are both
    /// `row_id` 1. A consumer keying its projection on `row_id` alone would
    /// merge them, silently, on every collection that holds one of each.
    ///
    /// Seen red by keying the projection below on `row_id` alone: the sealed
    /// delete removes the single.
    #[test]
    fn a_single_and_a_sealed_lot_can_share_a_row_id() {
        let (_dir, mut conn) = user_conn();

        let copy = collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        let lot = sealed::add(&conn, &new_sealed(5001)).unwrap();
        assert_eq!((copy, lot), (1, 1), "both tables start their ids at 1");

        sealed::delete(&conn, lot).unwrap();

        assert_eq!(
            source_ops(&conn),
            [
                ("collection".to_string(), "insert".to_string()),
                ("sealed_collection".to_string(), "insert".to_string()),
                ("sealed_collection".to_string(), "delete".to_string()),
            ],
            "the source table is what tells the two row 1s apart"
        );

        // What a consumer that respects the pair concludes, spelled out.
        let mut held: BTreeSet<(String, i64)> = BTreeSet::new();
        for ev in events(&conn) {
            match ev.op.as_str() {
                "insert" | "update" => {
                    held.insert((ev.source, ev.row_id));
                }
                "delete" => {
                    held.remove(&(ev.source, ev.row_id));
                }
                other => panic!("unknown op '{other}'"),
            }
        }
        assert_eq!(
            held,
            BTreeSet::from([("collection".to_string(), copy)]),
            "the single survives the sealed lot's delete"
        );
    }

    /// One sequence over both sources, so the events are ordered against
    /// each other rather than only within a table. A consumer replaying a
    /// mixed stream in `seq` order is replaying it in the order the
    /// mutations happened.
    #[test]
    fn the_two_sources_share_one_interleaved_sequence() {
        let (_dir, mut conn) = user_conn();

        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        sealed::add(&conn, &new_sealed(5001)).unwrap();
        collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        sealed::add(&conn, &new_sealed(5002)).unwrap();

        let evs = events(&conn);
        assert_eq!(
            evs.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [1, 2, 3, 4],
            "one sequence, no per-source numbering"
        );
        assert_eq!(
            evs.iter().map(|e| e.source.as_str()).collect::<Vec<_>>(),
            [
                "collection",
                "sealed_collection",
                "collection",
                "sealed_collection"
            ]
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

    /// And the same for sealed. `json_backup`'s restore writes
    /// `sealed_collection` with a column list of its own (pd-4gop) and the
    /// fixture seeder writes it directly; neither goes through `sealed::add`.
    #[test]
    fn a_raw_sql_writer_of_sealed_rows_is_covered_anyway() {
        let (_dir, conn) = user_conn();
        conn.execute(
            "INSERT INTO sealed_collection (product_id, quantity, added_at) \
             VALUES (5001, 2, '2026-08-14T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute("UPDATE sealed_collection SET notes = 'x'", [])
            .unwrap();
        conn.execute("DELETE FROM sealed_collection", []).unwrap();
        assert_eq!(
            source_ops(&conn),
            [
                ("sealed_collection".to_string(), "insert".to_string()),
                ("sealed_collection".to_string(), "update".to_string()),
                ("sealed_collection".to_string(), "delete".to_string()),
            ]
        );
        assert_eq!(events(&conn)[0].payload()["quantity"], 2);
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

        let evs = events(&conn);
        assert_eq!(ops(&conn), ["insert", "update"]);
        assert_eq!(evs[1].row_id, id);
        assert!(
            evs[1].payload()["binder_id"].is_null(),
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

    /// The same, for a sealed lot. The triggers are separate statements in
    /// the schema file, so the claim is separate too.
    #[test]
    fn a_rolled_back_sealed_mutation_leaves_no_event() {
        let (_dir, mut conn) = user_conn();
        let tx = conn.transaction().unwrap();
        tx.execute(
            "INSERT INTO sealed_collection (product_id, quantity, added_at) \
             VALUES (5001, 1, '2026-08-14T00:00:00Z')",
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
            .query_row("SELECT count(*) FROM sealed_collection", [], |r| r.get(0))
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
        assert_eq!(
            events(&conn)[0].seq,
            1,
            "the first surviving event is seq 1"
        );
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
    ///
    /// Half the writers hold singles and half hold sealed, because the
    /// sequence is shared: two tables contending for one AUTOINCREMENT is
    /// the arrangement that has to hold, not one table doing it alone.
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
                        if w % 2 == 0 {
                            conn.execute(
                                "INSERT INTO collection (printing_id, acquired_at, source) \
                                 VALUES (?1, '2026-08-13T00:00:00Z', 'test')",
                                [format!("p-{w}-{i}")],
                            )
                            .unwrap();
                        } else {
                            conn.execute(
                                "INSERT INTO sealed_collection \
                                   (product_id, quantity, added_at) \
                                 VALUES (?1, 1, '2026-08-14T00:00:00Z')",
                                [(w * EACH + i) as i64],
                            )
                            .unwrap();
                        }
                    }
                });
            }
        });

        let conn = crate::open_user(&path).unwrap();
        let evs = events(&conn);
        let seqs: Vec<i64> = evs.iter().map(|e| e.seq).collect();
        assert_eq!(seqs.len(), WRITERS * EACH);
        assert_eq!(
            seqs,
            (1..=(WRITERS * EACH) as i64).collect::<Vec<_>>(),
            "every event on its own number, in order, no gaps"
        );
        for source in SOURCES {
            assert_eq!(
                evs.iter().filter(|e| e.source == source).count(),
                WRITERS / 2 * EACH,
                "{source} wrote its half"
            );
        }
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
            events(&conn)[0].seq,
            3,
            "AUTOINCREMENT, so the next event continues past the trimmed rows"
        );
    }

    // -----------------------------------------------------------------
    // Not collection state
    // -----------------------------------------------------------------

    /// The exclusion is on the outbox alone. Both holdings tables are
    /// collection state and both are carried — a sealed lot silently missing
    /// from a backup is a lost holding, which is the same failure as a lost
    /// event pointed the other way.
    #[test]
    fn the_outbox_is_not_carried_by_the_json_envelope() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        sealed::add(&conn, &new_sealed(5001)).unwrap();
        assert_eq!(count(&conn), 2);

        let envelope: serde_json::Value =
            serde_json::from_str(&crate::json_backup::export(&conn).unwrap()).unwrap();
        assert!(
            envelope.get(TABLE).is_none(),
            "the envelope is collection state; the outbox is the log of \
             changes leaving it"
        );
        for source in SOURCES {
            assert_eq!(
                envelope
                    .get(source)
                    .and_then(|t| t.as_array())
                    .map(Vec::len),
                Some(1),
                "...and {source} itself is still there, with its row"
            );
        }
    }

    /// A restore is a mutation like any other: it empties the collection and
    /// fills it again, and both halves are recorded. The outbox that results
    /// describes the restored state — it is not the envelope's outbox,
    /// because the envelope has none.
    ///
    /// Both sources, because the restore writes both tables and the events
    /// have to describe both. `json_backup` empties every envelope table and
    /// re-inserts, so the sealed lot's clear-and-rewrite is exactly as
    /// visible as the single's.
    #[test]
    fn a_restore_records_itself_rather_than_restoring_a_stale_log() {
        let (_dir, mut conn) = user_conn();
        collection::add(&mut conn, &new_copy("sv3pt5-1-normal")).unwrap();
        sealed::add(&conn, &new_sealed(5001)).unwrap();
        let envelope = crate::json_backup::export(&conn).unwrap();

        collection::add(&mut conn, &new_copy("sv3pt5-1-holo")).unwrap();
        sealed::add(&conn, &new_sealed(5002)).unwrap();
        let before = count(&conn);
        assert_eq!(before, 4);

        crate::json_backup::import(
            &mut conn,
            &envelope,
            crate::json_backup::OnExisting::Replace,
        )
        .unwrap();

        // The import's order across tables is `known`'s (alphabetical), so
        // assert per source rather than on one interleaved list.
        let after = &events(&conn)[before as usize..];
        for source in SOURCES {
            let ops: Vec<&str> = after
                .iter()
                .filter(|e| e.source == source)
                .map(|e| e.op.as_str())
                .collect();
            assert_eq!(
                ops,
                ["delete", "delete", "insert"],
                "{source}: two rows cleared, the envelope's one written back"
            );
            let rows: i64 = conn
                .query_row(&format!("SELECT count(*) FROM \"{source}\""), [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(rows, 1, "{source}");
        }
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
        let lot = sealed::add(&conn, &new_sealed(5001)).unwrap();
        sealed::delete(&conn, lot).unwrap();
        assert_eq!(count(&conn), 4, "the two adds and the two deletes");

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
}
