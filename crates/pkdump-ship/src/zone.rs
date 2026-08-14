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
//! ## What it does NOT read
//!
//! Only `dataset=holdings`, and within it only events whose `source_table` is
//! `collection`. A part carrying another source table is counted and skipped
//! rather than merged: `row_id` is unique only within a table (`pd-4gop`), so
//! reducing two tables into one staging table would produce a plausible wrong
//! answer instead of an error.

use std::collections::BTreeMap;

use pkdump_db::outbox::Event;
use pkdump_keys::TenantKey;
use pkdump_lake::{ObjectSource, PART_SUFFIX, TenantDataset, TenantZoneConfig};
use rusqlite::Connection;

use crate::error::{Result, ShipError};
use crate::{cipher, encode};

/// The staging table Phase 3 reads. Created by [`materialize`], derived from
/// `collection`, and named here because two implementations spell it — this
/// one and the transform's SQL.
pub const HOLDINGS_TABLE: &str = "zone_holdings";

/// Where the rows in [`HOLDINGS_TABLE`] came from: which parts, how far
/// through the outbox, and when. Declared in `schema_user.sql`.
pub const HOLDINGS_RUN_TABLE: &str = "zone_holdings_run";

/// The source table whose events become holdings.
///
/// Not a filter that might widen: `sealed_collection` is a holding too
/// (`pd-4gop`), but a sealed lot is not a card and
/// `collection_value_snapshot` does not count one. When sealed gains its
/// triggers, valuing it is a decision with its own dimensions, not a table
/// name added here.
const HOLDINGS_SOURCE_TABLE: &str = "collection";

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
    /// Events whose `source_table` is not [`HOLDINGS_SOURCE_TABLE`]. Counted
    /// so a run that quietly ignored half the zone cannot look like a run
    /// that found nothing to ignore.
    pub other_tables: usize,
    /// The reduction: one entry per held row, keyed by its `row_id`.
    pub rows: BTreeMap<i64, serde_json::Value>,
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
            if event.source_table == HOLDINGS_SOURCE_TABLE {
                events.push(event);
            } else {
                out.other_tables += 1;
            }
        }
    }

    out.partitions.sort();
    out.partitions.dedup();
    out.rows = pkdump_db::outbox::project(&events)
        .into_iter()
        .map(|((_, row_id), payload)| (row_id, payload))
        .collect();
    Ok(out)
}

/// The `as_of=` component of a part key, if it has one.
fn as_of_of(object_key: &str) -> Option<String> {
    object_key
        .split('/')
        .find_map(|c| c.strip_prefix("as_of="))
        .map(str::to_string)
}

/// Replace [`HOLDINGS_TABLE`] in `conn` with `holdings`, and record the run.
///
/// One transaction: a Phase 3 run must never see half of one materialisation
/// and half of the last. Returns the rows written.
pub fn materialize(conn: &Connection, holdings: &ZoneHoldings, read_at: &str) -> Result<usize> {
    let columns = collection_columns(conn)?;
    let tx = conn.unchecked_transaction()?;

    tx.execute_batch(&format!(
        "DROP TABLE IF EXISTS {HOLDINGS_TABLE};
         CREATE TABLE {HOLDINGS_TABLE} AS SELECT * FROM {HOLDINGS_SOURCE_TABLE} WHERE 0;"
    ))?;

    // `json_extract` per column, so the payload's own types survive: an
    // INTEGER binder_id stays an integer and a NULL purchase_price stays
    // NULL. Building the row in Rust would mean deciding what a JSON number
    // is, which is the decision SQLite has already made for `collection`.
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
        "INSERT INTO {HOLDINGS_TABLE} ({quoted}) VALUES ({placeholders})"
    ))?;

    let mut written = 0usize;
    for payload in holdings.rows.values() {
        insert.execute([payload.to_string()])?;
        written += 1;
    }
    drop(insert);

    tx.execute(
        &format!(
            "INSERT OR REPLACE INTO {HOLDINGS_RUN_TABLE} \
                 (dataset, parts, events, max_seq, partitions, rows, read_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        ),
        rusqlite::params![
            TenantDataset::Holdings.as_str(),
            holdings.parts as i64,
            holdings.events as i64,
            holdings.max_seq,
            holdings.partitions.join(","),
            written as i64,
            read_at,
        ],
    )?;
    tx.commit()?;
    Ok(written)
}

/// `collection`'s column names, in declaration order.
fn collection_columns(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT name FROM pragma_table_info('{HOLDINGS_SOURCE_TABLE}')"
    ))?;
    let columns: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    if columns.is_empty() {
        return Err(ShipError::Zone(format!(
            "this database has no {HOLDINGS_SOURCE_TABLE} table, so there is no shape to \
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

    /// The staging table is `collection`'s shape, whatever that is today.
    #[test]
    fn the_staging_table_has_every_column_collection_has() {
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
        assert_eq!(of(HOLDINGS_TABLE), of("collection"));
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
            7,
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
                row_id,
                serde_json::json!({"id": row_id, "printing_id": "p", "acquired_at": "x", "source": "zone"}),
            );
        }
        materialize(&conn, &holdings, "2026-08-14T00:00:00Z").unwrap();
        holdings.rows.remove(&3);
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
            1,
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
