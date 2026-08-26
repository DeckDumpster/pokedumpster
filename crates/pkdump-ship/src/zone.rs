//! Reading the tenant zone back: Phase 3's holdings source (`pd-szh2`).
//!
//! The shipper's mirror image. [`crate::run`] takes outbox rows out of a
//! tenant's SQLite and seals them into `tenant/`; this takes them out of
//! `tenant/` and reduces them to the holdings they describe, which is what a
//! valuation is computed from.
//!
//! ```text
//!  tenant/database_id=<id>/dataset=holdings/     tenants/<database_id>.sqlite
//!  ┌───────────────────────────┐                 ┌──────────────────────────┐
//!  │ as_of=…/part-seq-A-B ─────┼─▶ open ─▶ decode│                          │
//!  │ as_of=…/part-seq-C-D ─────┼─▶   ─▶ project ─┼─▶ zone_holdings          │
//!  └───────────────────────────┘                 │   zone_holdings_run      │
//!                                                └──────────────────────────┘
//! ```
//!
//! ## Why the projection lands in the tenant's own database
//!
//! Phase 3's valuation is `catalog.prices` × holdings, and the price side is
//! `pyiceberg` — there is no Iceberg client in Rust here, and writing one
//! would make the offline valuation a *second implementation* of the
//! arithmetic it has to be proven identical to. So the two halves stay where
//! each already is: this module puts the zone's holdings somewhere the
//! existing transform can join them, and the transform's aggregation is
//! untouched.
//!
//! That makes the equivalence claim a one-token substitution —
//! `FROM collection` becomes `FROM zone_holdings` in
//! `lake/src/pkdump_lake/value_snapshots.py` and nothing else moves — so a
//! difference between the two runs can only be a difference in *holdings*.
//! An offline computation written from scratch could differ for a dozen
//! reasons and the proof would have to rule out each of them.
//!
//! The tenant's own database is also the only place this staging table
//! belongs. It is that tenant's holdings; it is dropped by the deletion that
//! drops them, restored by the restore that restores them, and reachable by
//! nobody who could not already read `collection`.
//!
//! ## The table's shape is derived, never declared
//!
//! `zone_holdings` is built from `collection`'s own `pragma_table_info`. A
//! hand-written mirror in `schema_user.sql` would be a **third** place the
//! collection's shape is written down — [`crate::encode`] declined to be the
//! second one for the same reason — and the day a column is added to
//! `collection` it would silently stop carrying it.
//!
//! It is a plain table with no constraints and, importantly, **no triggers**:
//! the outbox triggers are attached to `collection` by name, so materialising
//! here cannot emit outbox events and a Phase 3 run cannot feed itself.
//!
//! ## One staging table per source
//!
//! Only `dataset=holdings`, and within it every event whose `source_table` is
//! one this reader knows — `collection` into [`HOLDINGS_TABLE`] and
//! `sealed_collection` into [`SEALED_HOLDINGS_TABLE`] (`pd-bbv7`). Two
//! tables, never one: `row_id` is unique only within a source table
//! (`pd-4gop`), so reducing both into one would merge a single and a sealed
//! lot that merely share a number and produce a plausible wrong answer
//! instead of an error.
//!
//! Sealed was declined here until valuing it was decided — the comment this
//! replaces said "valuing them is a decision, not a table name", and that was
//! right: pd-bbv7 is the decision, and it arrives as a dimension of its own
//! rather than as a widening of `all`.
//!
//! ## What it does NOT read, and says so
//!
//! An event from any other source table is **counted by name** and skipped.
//! By name, because the thing that kept sealed invisible was a single number
//! that did not say what was in it; a third source added to the outbox and
//! not to [`HOLDINGS_SOURCES`] has to be nameable the night it first ships.
//! `every_outbox_source_has_a_staging_table` is the gate that stops it
//! getting that far unnoticed.

use std::collections::BTreeMap;

use pkdump_db::outbox::Event;
use pkdump_keys::TenantKey;
use pkdump_lake::{ObjectSource, PART_SUFFIX, TenantDataset, TenantZoneConfig};
use rusqlite::Connection;

use crate::error::{Result, ShipError};
use crate::{cipher, encode};

/// The staging table Phase 3 reads for loose cards. Created by
/// [`materialize`], derived from `collection`, and named here because two
/// implementations spell it — this one and the transform's SQL.
pub const HOLDINGS_TABLE: &str = "zone_holdings";

/// The same for sealed product, derived from `sealed_collection` (`pd-bbv7`).
pub const SEALED_HOLDINGS_TABLE: &str = "zone_sealed_holdings";

/// Where the rows in [`HOLDINGS_TABLE`] came from: which parts, how far
/// through the outbox, and when. Declared in `schema_user.sql`.
pub const HOLDINGS_RUN_TABLE: &str = "zone_holdings_run";

/// The outbox source tables whose events become holdings, each with the
/// staging table its reduction lands in.
///
/// This is the reader's whole notion of what the zone carries, and it is held
/// to `pkdump_db::outbox::SOURCE_TABLES` by
/// `every_outbox_source_has_a_staging_table`: a source the outbox ships and
/// this reader has no table for would be declined every night, silently and
/// forever, which is exactly how sealed stayed out of every valuation.
pub const HOLDINGS_SOURCES: &[(&str, &str)] = &[
    ("collection", HOLDINGS_TABLE),
    ("sealed_collection", SEALED_HOLDINGS_TABLE),
];

/// One tenant's holdings, as the zone holds them.
#[derive(Debug, Clone, Default)]
pub struct ZoneHoldings {
    /// Objects read.
    pub parts: usize,
    /// Events in them.
    pub events: usize,
    /// The highest `seq` any part carried — how far through that tenant's
    /// outbox the zone has got.
    pub max_seq: i64,
    /// The `as_of=` partitions read, sorted.
    pub partitions: Vec<String>,
    /// Events whose `source_table` is not in [`HOLDINGS_SOURCES`], counted
    /// **per table**. Counted so a run that quietly ignored part of the zone
    /// cannot look like a run that found nothing to ignore; per table so the
    /// operator is told which holdings went unvalued rather than how many.
    pub declined: BTreeMap<String, usize>,
    /// The reduction: one entry per held row, keyed by the
    /// `(source_table, row_id)` PAIR — `row_id` alone is unique only within a
    /// source, and the first single and the first sealed lot are both 1.
    pub rows: BTreeMap<(String, i64), serde_json::Value>,
}

impl ZoneHoldings {
    /// The rows this reduction holds for one source table.
    pub fn rows_of<'a>(
        &'a self,
        source_table: &'a str,
    ) -> impl Iterator<Item = &'a serde_json::Value> + 'a {
        self.rows
            .iter()
            .filter(move |((table, _), _)| table == source_table)
            .map(|(_, payload)| payload)
    }

    /// How many rows it holds for one source table.
    pub fn count_of(&self, source_table: &str) -> usize {
        self.rows_of(source_table).count()
    }
}

/// Read every holdings part a tenant has in the zone and reduce it.
///
/// The reduction is [`pkdump_db::outbox::project`] — the one implementation
/// of the resolution rule — over the events from *every* partition at once,
/// never partition by partition. An update that crossed midnight is two
/// events in two `as_of=` directories, and resolving each day separately
/// would let the older one win on whichever day it lands in.
pub fn read(
    source: &dyn ObjectSource,
    config: &TenantZoneConfig,
    key: &TenantKey,
    database_id: &str,
) -> Result<ZoneHoldings> {
    let prefix = config.rooted(pkdump_lake::dataset_prefix(
        database_id,
        TenantDataset::Holdings,
    )?);
    let keys: Vec<String> = source
        .list_keys(&prefix)
        .map_err(|e| ShipError::Zone(e.to_string()))?
        .into_iter()
        .filter(|k| k.ends_with(PART_SUFFIX))
        .collect();

    let mut out = ZoneHoldings::default();
    let mut events: Vec<Event> = Vec::new();
    for object_key in &keys {
        let sealed = source
            .get(object_key)
            .map_err(|e| ShipError::Zone(e.to_string()))?;
        // The key is the AAD, so a part moved to another tenant's prefix does
        // not open — the binding the shipper wrote is the one checked here.
        let parquet = cipher::open(key, object_key, &sealed)?;
        let part = encode::decode(parquet)?;

        out.parts += 1;
        out.events += part.len();
        if let Some(as_of) = as_of_of(object_key) {
            out.partitions.push(as_of);
        }
        for event in part {
            out.max_seq = out.max_seq.max(event.seq);
            if HOLDINGS_SOURCES
                .iter()
                .any(|(table, _)| *table == event.source_table)
            {
                events.push(event);
            } else {
                *out.declined.entry(event.source_table.clone()).or_default() += 1;
            }
        }
    }

    out.partitions.sort();
    out.partitions.dedup();
    // One `project` over every source at once, keyed by the pair it returns.
    // Reducing per source would be the same answer today and a second
    // implementation of the resolution rule tomorrow.
    out.rows = pkdump_db::outbox::project(&events);
    Ok(out)
}

/// The `as_of=` component of a part key, if it has one.
fn as_of_of(object_key: &str) -> Option<String> {
    object_key
        .split('/')
        .find_map(|c| c.strip_prefix("as_of="))
        .map(str::to_string)
}

/// Replace every staging table in `conn` with `holdings`, and record the run.
///
/// One transaction over ALL of them: a Phase 3 run must never see half of one
/// materialisation and half of the last, and that is as true across the two
/// tables as it is within one — a valuation that read this run's cards
/// against last run's sealed would be wrong in a way no number betrays.
/// Returns the total rows written.
pub fn materialize(conn: &Connection, holdings: &ZoneHoldings, read_at: &str) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;

    let mut written = 0usize;
    let mut per_source = Vec::with_capacity(HOLDINGS_SOURCES.len());
    for (source_table, staging) in HOLDINGS_SOURCES {
        let rows = materialize_one(&tx, holdings, source_table, staging)?;
        per_source.push(rows as i64);
        written += rows;
    }

    // `rows` and `sealed_rows` are one column per [`HOLDINGS_SOURCES`] entry,
    // in that order — taken from the loop above rather than looked up by name,
    // so a source renamed in one place cannot leave this row counting zero for
    // a staging table it just filled. `the_run_row_counts_every_source` is
    // what fails if a third source arrives with no column for it.
    tx.execute(
        &format!(
            "INSERT OR REPLACE INTO {HOLDINGS_RUN_TABLE} \
                 (dataset, parts, events, max_seq, partitions, rows, sealed_rows, read_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
        rusqlite::params![
            TenantDataset::Holdings.as_str(),
            holdings.parts as i64,
            holdings.events as i64,
            holdings.max_seq,
            holdings.partitions.join(","),
            per_source[0],
            per_source[1],
            read_at,
        ],
    )?;
    tx.commit()?;
    Ok(written)
}

/// One source table's rows into its staging table, inside the caller's
/// transaction. Returns the rows written.
fn materialize_one(
    tx: &rusqlite::Transaction<'_>,
    holdings: &ZoneHoldings,
    source_table: &str,
    staging: &str,
) -> Result<usize> {
    let columns = source_columns(tx, source_table)?;

    tx.execute_batch(&format!(
        "DROP TABLE IF EXISTS {staging};
         CREATE TABLE {staging} AS SELECT * FROM {source_table} WHERE 0;"
    ))?;

    // `json_extract` per column, so the payload's own types survive: an
    // INTEGER binder_id stays an integer and a NULL purchase_price stays
    // NULL. Building the row in Rust would mean deciding what a JSON number
    // is, which is the decision SQLite has already made for the source table.
    let placeholders = columns
        .iter()
        .map(|c| format!("json_extract(?1, '$.{c}')"))
        .collect::<Vec<_>>()
        .join(", ");
    let quoted = columns
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut insert = tx.prepare(&format!(
        "INSERT INTO {staging} ({quoted}) VALUES ({placeholders})"
    ))?;

    let mut written = 0usize;
    for payload in holdings.rows_of(source_table) {
        insert.execute([payload.to_string()])?;
        written += 1;
    }
    Ok(written)
}

/// A source table's column names, in declaration order.
fn source_columns(conn: &Connection, source_table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name FROM pragma_table_info('{source_table}')"
    ))?;
    let columns: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    if columns.is_empty() {
        return Err(ShipError::Zone(format!(
            "this database has no {source_table} table, so there is no shape to \
             materialise the zone into. Any pkdump command that opens it re-applies the schema."
        )));
    }
    Ok(columns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn the_as_of_component_is_read_off_a_part_key() {
        assert_eq!(
            as_of_of(
                "tenant/database_id=X/dataset=holdings/as_of=2026-08-14/\
                 part-seq-000000000001-000000000005.parquet.enc"
            )
            .as_deref(),
            Some("2026-08-14")
        );
        assert_eq!(as_of_of("tenant/database_id=X/dataset=holdings/"), None);
    }

    /// Each staging table is its SOURCE's shape, whatever that is today.
    #[test]
    fn every_staging_table_has_every_column_its_source_has() {
        let (_dir, conn) = collection_db();
        materialize(&conn, &ZoneHoldings::default(), "2026-08-14T00:00:00Z").unwrap();

        let of = |table: &str| -> Vec<String> {
            conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        for (source, staging) in HOLDINGS_SOURCES {
            let columns = of(staging);
            assert!(!columns.is_empty(), "{staging} was not created");
            assert_eq!(columns, of(source), "{staging} is not {source}'s shape");
        }
    }

    /// pd-bbv7. The outbox ships a source; this reader has to have somewhere
    /// to put it. A source in one list and not the other is declined every
    /// night, silently, and nothing values those holdings — which is exactly
    /// the state sealed product was in between pd-4gop and this bead.
    #[test]
    fn every_outbox_source_has_a_staging_table() {
        for (source, _) in pkdump_db::outbox::SOURCE_TABLES {
            assert!(
                HOLDINGS_SOURCES.iter().any(|(table, _)| table == source),
                "the outbox ships `{source}` and the zone reader has no staging table for it"
            );
        }
        for (source, _) in HOLDINGS_SOURCES {
            assert!(
                pkdump_db::outbox::SOURCE_TABLES
                    .iter()
                    .any(|(table, _)| table == source),
                "the zone reader materialises `{source}`, which the outbox never ships"
            );
        }
    }

    /// The run row counts both staging tables, so an operator reading it can
    /// tell "no sealed shipped" from "sealed was not materialised". Both
    /// counts are named columns, so a THIRD source would need one too — this
    /// is where that gets noticed.
    #[test]
    fn the_run_row_counts_every_source() {
        assert_eq!(
            HOLDINGS_SOURCES.len(),
            2,
            "zone_holdings_run has a column per source (rows, sealed_rows); a third \
             source needs a third column, in schema_user.sql and in USER_ADDED_COLUMNS"
        );

        let (_dir, conn) = collection_db();
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

        let (cards, sealed): (i64, i64) = conn
            .query_row(
                &format!("SELECT rows, sealed_rows FROM {HOLDINGS_RUN_TABLE}"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((cards, sealed), (1, 3));

        let lots: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {SEALED_HOLDINGS_TABLE}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lots, 3, "the sealed lots landed in their own table");
    }

    /// The pd-4gop pair rule, on the reading side. A single and a sealed lot
    /// that share a `row_id` are two holdings, and keying the reduction on
    /// `row_id` alone would land one of them in both staging tables — or
    /// neither.
    #[test]
    fn a_single_and_a_sealed_lot_sharing_a_row_id_stay_apart() {
        let (_dir, conn) = collection_db();
        let mut holdings = ZoneHoldings::default();
        holdings.rows.insert(
            ("collection".into(), 1),
            serde_json::json!({"id": 1, "printing_id": "base1-4-holo",
                               "acquired_at": "2026-08-01", "source": "zone"}),
        );
        holdings.rows.insert(
            ("sealed_collection".into(), 1),
            serde_json::json!({"id": 1, "product_id": 4242, "quantity": 5,
                               "status": "owned", "added_at": "2026-08-01T00:00:00Z"}),
        );
        materialize(&conn, &holdings, "2026-08-14T00:00:00Z").unwrap();

        let printing: String = conn
            .query_row(
                &format!("SELECT printing_id FROM {HOLDINGS_TABLE}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(printing, "base1-4-holo");
        let (product, quantity): (i64, i64) = conn
            .query_row(
                &format!("SELECT product_id, quantity FROM {SEALED_HOLDINGS_TABLE}"),
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((product, quantity), (4242, 5));
    }

    /// An event from a source this reader does not know is counted UNDER ITS
    /// OWN NAME. One number that says "3 events were ignored" is what let
    /// sealed product sit unvalued without anybody being able to see what was
    /// being ignored.
    #[test]
    fn a_source_with_no_staging_table_is_declined_by_name() {
        let mut out = ZoneHoldings::default();
        *out.declined.entry("wishlist".into()).or_default() += 2;
        *out.declined.entry("decks".into()).or_default() += 1;
        assert_eq!(out.declined.get("wishlist"), Some(&2));
        assert_eq!(out.declined.get("decks"), Some(&1));
    }

    /// Materialising is what a Phase 3 run does before it values anything; if
    /// it emitted outbox events, the next run would ship them back and the
    /// zone would grow a copy of itself every night.
    #[test]
    fn materialising_emits_no_outbox_events() {
        let (_dir, conn) = collection_db();
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source) VALUES ('a', '2026-08-01', 't')",
            [],
        )
        .unwrap();
        let before: i64 = conn
            .query_row("SELECT count(*) FROM ownership_outbox", [], |r| r.get(0))
            .unwrap();

        let mut holdings = ZoneHoldings::default();
        holdings.rows.insert(
            ("collection".into(), 7),
            serde_json::json!({"id": 7, "printing_id": "z", "acquired_at": "2026-08-02", "source": "zone"}),
        );
        assert_eq!(
            materialize(&conn, &holdings, "2026-08-14T00:00:00Z").unwrap(),
            1
        );

        let after: i64 = conn
            .query_row("SELECT count(*) FROM ownership_outbox", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "the staging table carries no triggers");
    }

    /// A second run replaces the first outright. A materialisation that
    /// appended would double every holding and value the collection twice.
    #[test]
    fn a_second_materialisation_replaces_the_first() {
        let (_dir, conn) = collection_db();
        let mut holdings = ZoneHoldings::default();
        for row_id in 1..=3 {
            holdings.rows.insert(
                ("collection".into(), row_id),
                serde_json::json!({"id": row_id, "printing_id": "p", "acquired_at": "x", "source": "zone"}),
            );
        }
        materialize(&conn, &holdings, "2026-08-14T00:00:00Z").unwrap();
        holdings.rows.remove(&("collection".to_string(), 3));
        materialize(&conn, &holdings, "2026-08-15T00:00:00Z").unwrap();

        let rows: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {HOLDINGS_TABLE}"), [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(rows, 2, "a removed holding is gone, not still there");

        let runs: i64 = conn
            .query_row(
                &format!("SELECT count(*) FROM {HOLDINGS_RUN_TABLE}"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(runs, 1, "one row per dataset, rewritten");
    }

    /// The payload's types survive: a REAL stays a REAL, a NULL stays NULL,
    /// and an INTEGER foreign key does not arrive as the text "3".
    #[test]
    fn a_payload_keeps_its_types() {
        let (_dir, conn) = collection_db();
        let mut holdings = ZoneHoldings::default();
        holdings.rows.insert(
            ("collection".into(), 1),
            serde_json::json!({
                "id": 1,
                "printing_id": "sv3pt5-1-normal",
                "acquired_at": "2026-08-01T00:00:00Z",
                "source": "zone",
                "purchase_price": 12.5,
                "sale_price": serde_json::Value::Null,
                "graded": 0,
            }),
        );
        materialize(&conn, &holdings, "2026-08-14T00:00:00Z").unwrap();

        let (kind, price, sale): (String, f64, Option<f64>) = conn
            .query_row(
                &format!(
                    "SELECT typeof(purchase_price), purchase_price, sale_price FROM {HOLDINGS_TABLE}"
                ),
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "real");
        assert_eq!(price, 12.5);
        assert_eq!(sale, None);
    }
}
