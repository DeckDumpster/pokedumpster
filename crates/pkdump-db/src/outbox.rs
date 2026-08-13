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
//! This module holds the one name the rest of the crate needs
//! ([`TABLE`]) and the tests that hold the contract in place. It does not
//! read the outbox: the consumer is the shipper, which is its own change
//! (item 4 of the inbound-leg epic, pd-dxn3).
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

/// The outbox table's name. The one place it is spelled in Rust.
pub const TABLE: &str = "ownership_outbox";

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

    /// `(seq, op, row_id, payload)` for every outbox row, in sequence order.
    fn events(conn: &Connection) -> Vec<(i64, String, i64, String)> {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT seq, op, row_id, payload FROM {TABLE} ORDER BY seq"
            ))
            .unwrap();
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap();
        rows.collect::<rusqlite::Result<_>>().unwrap()
    }

    fn ops(conn: &Connection) -> Vec<String> {
        events(conn).into_iter().map(|(_, op, _, _)| op).collect()
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
        let (seq, op, row_id, payload) = &evs[0];
        assert_eq!(*seq, 1);
        assert_eq!(op, "insert");
        assert_eq!(*row_id, id);

        let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
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
        let payload: serde_json::Value = serde_json::from_str(&evs[1].3).unwrap();
        assert_eq!(
            payload["printing_id"], "sv3pt5-1-holo",
            "a delete records the pre-image — the row is gone, so nothing \
             else can say what was lost"
        );
        assert_eq!(evs[1].2, id);
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

        let evs = events(&conn);
        assert_eq!(evs.len(), 3);
        for (seq, op, _, payload) in &evs {
            let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
            let carried: Vec<&String> = payload.as_object().unwrap().keys().collect();
            let missing: Vec<&String> = declared
                .iter()
                .filter(|c| !carried.contains(c))
                .collect();
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

        let evs = events(&conn);
        assert_eq!(ops(&conn), ["insert", "update"]);
        assert_eq!(evs[1].2, id);
        let payload: serde_json::Value = serde_json::from_str(&evs[1].3).unwrap();
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
        assert_eq!(events(&conn)[0].0, 1, "the first surviving event is seq 1");
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
        let seqs: Vec<i64> = events(&conn).into_iter().map(|(s, _, _, _)| s).collect();
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
            events(&conn)[0].0,
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
}
