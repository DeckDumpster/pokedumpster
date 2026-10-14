//! What the kit's shipper cannot check about this site, because it is a claim about this
//! site's schema.
//!
//! `dumpster-kit-ship` is tested against a fixture catalog of its own: two holdings tables and
//! their outbox triggers. Everything below is the same claim restated over PokeDumpster's REAL
//! collection schema (`pkdump_db::open_user`), which is the only place a drift between "the
//! tables the outbox carries" and "the tables the zone reader stages" can show. Before the
//! cutover these lived in the shipper's own unit tests; the shipper moved and the schema did
//! not, so they came here.

use pkdump_db::outbox::{CURSOR_TABLE, GAP_TABLE, SOURCE_TABLES, TRANSPORT_TABLES};
use pkdump_ship::{HOLDINGS_RUN_TABLE, HOLDINGS_SOURCES, ZoneHoldings, cursor, materialize};
use rusqlite::Connection;

fn user_db() -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let conn = pkdump_db::open_user(&dir.path().join("collection.sqlite")).unwrap();
    (dir, conn)
}

fn columns(conn: &Connection, table: &str) -> Vec<String> {
    conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

/// pd-bbv7. The outbox ships a source; the reader has to have somewhere to put it. A source in
/// one list and not the other is declined every night, silently, and nothing values those
/// holdings — which is exactly the state sealed product was in between pd-4gop and pd-bbv7.
#[test]
fn every_outbox_source_has_a_staging_table() {
    for (source, _) in SOURCE_TABLES {
        assert!(
            HOLDINGS_SOURCES.iter().any(|(table, _)| table == source),
            "the outbox ships `{source}` and the zone reader has no staging table for it"
        );
    }
    for (source, _) in HOLDINGS_SOURCES {
        assert!(
            SOURCE_TABLES.iter().any(|(table, _)| table == source),
            "the zone reader materialises `{source}`, which the outbox never ships"
        );
    }
}

/// Each staging table is its SOURCE's shape, whatever that is today.
#[test]
fn every_staging_table_has_every_column_its_source_has() {
    let (_dir, conn) = user_db();
    materialize(&conn, &ZoneHoldings::default(), "2026-08-14T00:00:00Z").unwrap();

    for (source, staging) in HOLDINGS_SOURCES {
        let staged = columns(&conn, staging);
        assert!(!staged.is_empty(), "{staging} was not created");
        assert_eq!(
            staged,
            columns(&conn, source),
            "{staging} is not {source}'s shape"
        );
    }
}

/// The staging tables and their provenance row are transport state: a restored envelope must
/// neither carry them nor clear them. A staging table missing from this list would start
/// restoring somebody else's staged zone read into a fresh database.
#[test]
fn the_staging_tables_are_transport_state() {
    for (_, staging) in HOLDINGS_SOURCES {
        assert!(
            TRANSPORT_TABLES.contains(staging),
            "{staging} is staged by the zone read-back but is not in TRANSPORT_TABLES"
        );
    }
    assert!(TRANSPORT_TABLES.contains(&HOLDINGS_RUN_TABLE));
}

/// The run row counts every source the reader stages, in the one transaction that staged them.
/// This is what the transform reads to tell "no sealed shipped" from "sealed was not
/// materialised", and the kit writes it as a JSON object keyed by source table — so the claim
/// is that the keys are exactly this site's sources.
#[test]
fn the_run_row_counts_every_source() {
    let (_dir, conn) = user_db();
    let mut holdings = ZoneHoldings::default();
    holdings.rows.insert(
        ("collection".into(), 1),
        serde_json::json!({"id": 1, "printing_id": "p", "acquired_at": "x", "source": "zone"}),
    );
    for row_id in 1..=3 {
        holdings.rows.insert(
            ("sealed_collection".into(), row_id),
            serde_json::json!({"id": row_id, "product_id": 900 + row_id, "quantity": 2,
                               "status": "owned", "added_at": "2026-08-01T00:00:00Z"}),
        );
    }
    assert_eq!(
        materialize(&conn, &holdings, "2026-08-14T00:00:00Z").unwrap(),
        4
    );

    let (rows, per_source): (i64, String) = conn
        .query_row(
            &format!("SELECT rows, per_source FROM {HOLDINGS_RUN_TABLE}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rows, 4, "`rows` is every holding the reduction produced");
    let counts: std::collections::BTreeMap<String, i64> =
        serde_json::from_str(&per_source).unwrap();
    assert_eq!(
        counts.keys().cloned().collect::<Vec<_>>(),
        {
            let mut tables: Vec<String> = HOLDINGS_SOURCES
                .iter()
                .map(|(t, _)| t.to_string())
                .collect();
            tables.sort();
            tables
        },
        "one count per source this site stages, and no other"
    );
    assert_eq!((counts["collection"], counts["sealed_collection"]), (1, 3));
}

/// Materialising is what a Phase 3 run does before it values anything; if it emitted outbox
/// events, the next run would ship them back and the zone would grow a copy of itself every
/// night. The triggers are this site's, so the claim is too.
#[test]
fn materialising_emits_no_outbox_events() {
    let (_dir, conn) = user_db();
    conn.execute(
        "INSERT INTO collection (printing_id, acquired_at, source) VALUES ('a', '2026-08-01', 't')",
        [],
    )
    .unwrap();
    let count = |conn: &Connection| -> i64 {
        conn.query_row("SELECT count(*) FROM ownership_outbox", [], |r| r.get(0))
            .unwrap()
    };
    let before = count(&conn);

    let mut holdings = ZoneHoldings::default();
    holdings.rows.insert(
        ("collection".into(), 7),
        serde_json::json!({"id": 7, "printing_id": "z", "acquired_at": "2026-08-02", "source": "zone"}),
    );
    assert_eq!(
        materialize(&conn, &holdings, "2026-08-14T00:00:00Z").unwrap(),
        1
    );
    assert_eq!(
        before,
        count(&conn),
        "the staging table carries no triggers"
    );
}

/// The exclusion pd-5m54 established, extended to the two tables the shipper adds: a
/// collection's shipping position is not part of the collection, and an envelope that carried
/// it would let a restore skip events it had never shipped.
#[test]
fn the_cursor_and_the_ledger_are_not_carried_by_the_json_envelope() {
    let (_dir, conn) = user_db();
    cursor::advance(&conn, 12).unwrap();
    cursor::record_gaps(
        &conn,
        &[pkdump_ship::Gap {
            from_seq: 3,
            to_seq: 4,
        }],
    )
    .unwrap();

    let envelope: serde_json::Value =
        serde_json::from_str(&pkdump_db::json_backup::export(&conn).unwrap()).unwrap();
    assert!(envelope.get(CURSOR_TABLE).is_none(), "the cursor travelled");
    assert!(
        envelope.get(GAP_TABLE).is_none(),
        "the gap ledger travelled"
    );
}

/// …and the other direction: an import must not clear them either. The events a restore itself
/// writes are unshipped, and a cursor wiped by the restore would re-ship the whole history
/// instead.
#[test]
fn an_import_leaves_the_cursor_and_the_ledger_where_they_were() {
    let (_dir, mut conn) = user_db();
    cursor::advance(&conn, 12).unwrap();
    cursor::record_gaps(
        &conn,
        &[pkdump_ship::Gap {
            from_seq: 3,
            to_seq: 4,
        }],
    )
    .unwrap();
    let envelope = pkdump_db::json_backup::export(&conn).unwrap();

    pkdump_db::json_backup::import(
        &mut conn,
        &envelope,
        pkdump_db::json_backup::OnExisting::Replace,
    )
    .unwrap();

    assert_eq!(cursor::shipped_thru(&conn).unwrap(), 12);
    assert_eq!(cursor::gaps(&conn).unwrap().len(), 1);
}

/// The upgrade every existing box takes. A collection read back by the build before the kit
/// has a `zone_holdings_run` with `rows, sealed_rows` and nothing else; opening it must add
/// `per_source`, the kit's read-back must then write its row, and a row the old build wrote
/// must read as "counted differently" rather than as zero sealed lots.
#[test]
fn a_collection_read_back_before_the_kit_gains_per_source_and_reads_back_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("collection.sqlite");
    drop(pkdump_db::open_user(&path).unwrap());

    // Put the table back the way the previous build declared it, with its row.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "DROP TABLE zone_holdings_run;
         CREATE TABLE zone_holdings_run (
             dataset TEXT PRIMARY KEY, parts INTEGER NOT NULL, events INTEGER NOT NULL,
             max_seq INTEGER NOT NULL, partitions TEXT NOT NULL, rows INTEGER NOT NULL,
             read_at TEXT NOT NULL, sealed_rows INTEGER NOT NULL DEFAULT 0);
         INSERT INTO zone_holdings_run VALUES ('holdings', 1, 5, 5, '2026-08-01', 4, 'then', 2);",
    )
    .unwrap();
    drop(conn);

    let conn = pkdump_db::open_user(&path).unwrap();
    assert!(
        columns(&conn, HOLDINGS_RUN_TABLE).contains(&"per_source".to_string()),
        "opening the collection did not add per_source"
    );
    let old: String = conn
        .query_row(
            &format!("SELECT per_source FROM {HOLDINGS_RUN_TABLE}"),
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        old, "{}",
        "the old build's row says nothing about its sources"
    );

    materialize(&conn, &ZoneHoldings::default(), "2026-08-14T00:00:00Z").unwrap();
    let (rows, per_source): (i64, String) = conn
        .query_row(
            &format!("SELECT rows, per_source FROM {HOLDINGS_RUN_TABLE}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(per_source, r#"{"collection":0,"sealed_collection":0}"#);
}
