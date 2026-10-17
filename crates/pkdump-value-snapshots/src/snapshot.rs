//! One tenant's snapshot, written into that tenant's own database.
//!
//! A transliteration of `pkdump_lake.value_snapshots` — the SQL is carried
//! over verbatim, because the arithmetic must be *observably* the same as
//! `value_history.rs` (and as the Python it replaces) before the timer is
//! pointed here. See that module for the reasoning behind every rule; what is
//! restated here is only what a reader of this file needs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};

use crate::Refusal;
use crate::lake::{CardPrices, SealedPrices};

/// The staging tables the shipper's read-back writes (spelled in three places
/// besides this one; `tests/lake/phase3.sh` holds them together).
pub const ZONE_HOLDINGS: &str = "zone_holdings";
pub const ZONE_SEALED_HOLDINGS: &str = "zone_sealed_holdings";
pub const ZONE_HOLDINGS_RUN: &str = "zone_holdings_run";

pub const REGISTRY_FILE: &str = "registry.sqlite";
pub const TENANTS_DIR: &str = "tenants";

#[derive(Debug, Clone)]
pub struct Tenant {
    pub handle: String,
    pub database_id: String,
    pub path: PathBuf,
}

fn owned_sql() -> String {
    format!(
        "
CREATE TEMP TABLE _snap_owned AS
SELECT c.id,
       c.purchase_price,
       c.binder_id,
       cd.set_code,
       COALESCE(cond.multiplier, 1.0) AS mult,
       COALESCE(
         (SELECT lp.price FROM _lake_prices lp
            WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id
              AND lp.sub_type_name = p.sub_type_name
            LIMIT 1),
         (SELECT o.price FROM shared.catalog_price_overrides o
            WHERE o.printing_id = p.printing_id),
         (SELECT mp.price FROM manual_prices mp
            WHERE mp.printing_id = p.printing_id
              AND EXISTS (SELECT 1 FROM user_printings up
                           WHERE up.printing_id = mp.printing_id)
            ORDER BY mp.observed_at DESC LIMIT 1)
       ) AS market_price
  FROM {ZONE_HOLDINGS} c
  JOIN (
         SELECT printing_id, card_id, tcgplayer_product_id, sub_type_name
           FROM shared.printings
         UNION ALL
         SELECT printing_id, card_id, NULL, NULL
           FROM user_printings
       ) p ON c.printing_id = p.printing_id
  JOIN shared.cards cd ON p.card_id = cd.card_id
  LEFT JOIN conditions cond ON cond.name = c.condition
 WHERE c.status = 'owned';
"
    )
}

fn owned_sealed_sql() -> String {
    format!(
        "
CREATE TEMP TABLE _snap_sealed AS
SELECT sc.id,
       sc.quantity,
       sc.purchase_price,
       (SELECT lsp.price FROM _lake_sealed_prices lsp
          WHERE lsp.tcgplayer_product_id = sc.product_id LIMIT 1) AS market_price
  FROM {ZONE_SEALED_HOLDINGS} sc
 WHERE sc.status = 'owned';
"
    )
}

/// The four aggregates, in the order `value_history::insert_dimensions` writes
/// them.
const DIMENSION_SQL: [&str; 4] = [
    "INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
     SELECT ?, 'all', NULL,
            COALESCE(SUM(market_price * mult), 0.0),
            COALESCE(SUM(purchase_price), 0.0),
            COUNT(*)
       FROM _snap_owned",
    "INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
     SELECT ?, 'set', set_code,
            COALESCE(SUM(market_price * mult), 0.0),
            COALESCE(SUM(purchase_price), 0.0),
            COUNT(*)
       FROM _snap_owned
      GROUP BY set_code",
    "INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
     SELECT ?, 'binder', CAST(binder_id AS TEXT),
            COALESCE(SUM(market_price * mult), 0.0),
            COALESCE(SUM(purchase_price), 0.0),
            COUNT(*)
       FROM _snap_owned
      WHERE binder_id IS NOT NULL
      GROUP BY binder_id",
    // `HAVING COUNT(*) > 0`: a tenant who owns no sealed product gets no
    // sealed row at all, exactly as `set` and `binder` behave.
    "INSERT INTO collection_value_snapshot
        (date, dimension, bucket, market_value, cost_basis, card_count)
     SELECT ?, 'sealed', NULL,
            COALESCE(SUM(market_price * quantity), 0.0),
            COALESCE(SUM(purchase_price * quantity), 0.0),
            COALESCE(SUM(quantity), 0)
       FROM _snap_sealed
     HAVING COUNT(*) > 0",
];

/// Every **active** user in the registry, in registry order. A detached user is
/// not included.
pub fn tenants(root: &Path, only: Option<&[String]>) -> Result<Vec<Tenant>, Refusal> {
    let registry = root.join(REGISTRY_FILE);
    if !registry.exists() {
        return Err(Refusal(format!(
            "no registry at {} — this data directory predates the opaque-id layout. \
             Run `pkdump tenant migrate` (see deploy/TENANTS.md) and re-run.",
            registry.display()
        )));
    }
    // Not read-only: a WAL database cannot be opened that way when SQLite has
    // to manage the -shm. `query_only` is the honest equivalent.
    let conn = Connection::open(&registry)?;
    conn.execute_batch("PRAGMA query_only = ON")?;
    let mut stmt = conn.prepare(
        "SELECT handle, database_id FROM user WHERE state = 'active' ORDER BY created_at, handle",
    )?;
    let found: Vec<Tenant> = stmt
        .query_map([], |r| {
            let handle: String = r.get(0)?;
            let database_id: String = r.get(1)?;
            Ok(Tenant {
                path: root.join(TENANTS_DIR).join(format!("{database_id}.sqlite")),
                handle,
                database_id,
            })
        })?
        .collect::<Result<_, _>>()?;

    let Some(only) = only else { return Ok(found) };
    let by_handle: HashMap<&str, &Tenant> = found.iter().map(|t| (t.handle.as_str(), t)).collect();
    let missing: Vec<&str> = only
        .iter()
        .map(String::as_str)
        .filter(|h| !by_handle.contains_key(h))
        .collect();
    if !missing.is_empty() {
        return Err(Refusal(format!(
            "no active user for handle(s) {} — `pkdump tenant list` shows who is registered",
            missing.join(", ")
        )));
    }
    Ok(only.iter().map(|h| by_handle[h.as_str()].clone()).collect())
}

fn has_table(conn: &Connection, name: &str) -> Result<bool, Refusal> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        [name],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

fn require_provenance_table(conn: &Connection) -> Result<(), Refusal> {
    if !has_table(conn, "collection_value_snapshot_run")? {
        return Err(Refusal(
            "this database has no collection_value_snapshot_run table — it was last opened by a \
             build that predates pd-ruwh. Any pkdump command that opens it \
             (e.g. `pkdump tenant list`) re-applies the schema."
                .into(),
        ));
    }
    Ok(())
}

/// Refuse a tenant whose staged zone read is missing or behind the zone. See
/// the Python's docstring: a stale read values today's collection at older
/// holdings and every number looks reasonable.
fn require_zone_holdings(conn: &Connection, tenant: &Tenant) -> Result<String, Refusal> {
    let missing: Vec<&str> = [ZONE_HOLDINGS, ZONE_SEALED_HOLDINGS]
        .into_iter()
        .filter(|t| !has_table(conn, t).unwrap_or(false))
        .collect();
    if !missing.is_empty() {
        return Err(Refusal(format!(
            "{} has no {} — run `pkdump-ship holdings --tenant {}` with a build that carries \
             pd-bbv7 (deploy/TENANT_ZONE.md).",
            tenant.handle,
            missing.join(", "),
            tenant.database_id
        )));
    }

    let row: Option<(i64, String, i64, String)> = conn
        .query_row(
            &format!("SELECT max_seq, read_at, parts, per_source FROM {ZONE_HOLDINGS_RUN} WHERE dataset = ?"),
            ["holdings"],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    let Some((max_seq, read_at, parts, per_source)) = row else {
        return Err(Refusal(format!(
            "{} has no staged zone holdings — run `pkdump-ship holdings --tenant {}` first \
             (deploy/TENANT_ZONE.md). Valuing from a missing table would be valuing from \
             nothing; valuing from a stale one would be worse, so neither is guessed at.",
            tenant.handle, tenant.database_id
        )));
    };

    let counts: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&per_source)
        .map_err(|e| {
            Refusal(format!(
                "{}'s zone_holdings_run.per_source is not JSON: {e}",
                tenant.handle
            ))
        })?;
    let (Some(rows), Some(sealed_rows)) = (
        counts.get("collection").and_then(|v| v.as_i64()),
        counts.get("sealed_collection").and_then(|v| v.as_i64()),
    ) else {
        let mut keys: Vec<&String> = counts.keys().collect();
        keys.sort();
        return Err(Refusal(format!(
            "{}'s zone_holdings_run counts {} but the transform needs collection and \
             sealed_collection — it was written by a build that counted differently. Re-run \
             `pkdump-ship holdings --tenant {}` (deploy/TENANT_ZONE.md).",
            tenant.handle,
            if keys.is_empty() {
                "no source".to_string()
            } else {
                format!("{keys:?}")
            },
            tenant.database_id
        )));
    };

    let shipped_thru: i64 = conn
        .query_row(
            "SELECT shipped_thru FROM ownership_outbox_cursor WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if shipped_thru > max_seq {
        return Err(Refusal(format!(
            "{}'s staged zone holdings stop at seq {max_seq} but the shipper has put \
             {shipped_thru} into the zone — the materialisation ({read_at}) predates the last \
             ship, so these numbers would be yesterday's holdings at today's prices. \
             Re-run `pkdump-ship holdings --tenant {}`.",
            tenant.handle, tenant.database_id
        )));
    }
    let staged_sealed: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM {ZONE_SEALED_HOLDINGS}"),
        [],
        |r| r.get(0),
    )?;
    if staged_sealed != sealed_rows {
        return Err(Refusal(format!(
            "{}'s {ZONE_SEALED_HOLDINGS} holds {staged_sealed} lot(s) but the read that filled \
             {ZONE_HOLDINGS} recorded {sealed_rows} — the two were not materialised together, so \
             these sealed lots belong to an earlier read of the zone. Re-run \
             `pkdump-ship holdings --tenant {}`.",
            tenant.handle, tenant.database_id
        )));
    }

    println!(
        "    zone: {rows} card(s) + {sealed_rows} sealed lot(s) from {parts} part(s) through \
         seq {max_seq}, read {read_at}"
    );
    Ok(read_at)
}

/// Stage the prices this tenant can use, restricted to the products it owns.
fn stage_prices(conn: &Connection, prices: &CardPrices) -> Result<usize, Refusal> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp._lake_prices;
         CREATE TEMP TABLE _lake_prices (
           tcgplayer_product_id INTEGER NOT NULL,
           sub_type_name        TEXT NOT NULL,
           price                REAL NOT NULL,
           PRIMARY KEY (tcgplayer_product_id, sub_type_name)
         )",
    )?;
    let mut owned = conn.prepare(&format!(
        "SELECT DISTINCT p.tcgplayer_product_id, p.sub_type_name \
           FROM {ZONE_HOLDINGS} c \
           JOIN shared.printings p ON c.printing_id = p.printing_id \
          WHERE c.status = 'owned' AND p.tcgplayer_product_id IS NOT NULL"
    ))?;
    let keys: Vec<(i64, String)> = owned
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut insert = conn.prepare("INSERT INTO _lake_prices VALUES (?, ?, ?)")?;
    let mut staged = 0;
    for key in keys {
        if let Some(price) = prices.get(&key) {
            insert.execute(params![key.0, key.1, price])?;
            staged += 1;
        }
    }
    Ok(staged)
}

fn stage_sealed_prices(conn: &Connection, prices: &SealedPrices) -> Result<usize, Refusal> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp._lake_sealed_prices;
         CREATE TEMP TABLE _lake_sealed_prices (
           tcgplayer_product_id INTEGER PRIMARY KEY,
           price                REAL NOT NULL
         )",
    )?;
    let mut owned = conn.prepare(&format!(
        "SELECT DISTINCT product_id FROM {ZONE_SEALED_HOLDINGS} WHERE status = 'owned'"
    ))?;
    let ids: Vec<i64> = owned
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let mut insert = conn.prepare("INSERT INTO _lake_sealed_prices VALUES (?, ?)")?;
    let mut staged = 0;
    for id in ids {
        if let Some(price) = prices.get(&id) {
            insert.execute(params![id, price])?;
            staged += 1;
        }
    }
    Ok(staged)
}

/// Compute one tenant's snapshot for `date`; returns the rows written (or that
/// would have been, under `dry_run`).
#[allow(clippy::too_many_arguments)]
pub fn snapshot(
    tenant: &Tenant,
    shared: &Path,
    prices: &CardPrices,
    sealed_prices: &SealedPrices,
    date: &str,
    identifier: &str,
    lake_ref: &str,
    dry_run: bool,
) -> Result<usize, Refusal> {
    if !tenant.path.exists() {
        return Err(Refusal(format!("no database at {}", tenant.path.display())));
    }
    let conn = Connection::open(&tenant.path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.execute(
        "ATTACH DATABASE ? AS shared",
        [shared.to_string_lossy().as_ref()],
    )?;
    require_provenance_table(&conn)?;
    require_zone_holdings(&conn, tenant)?;

    conn.execute_batch("BEGIN IMMEDIATE")?;
    let outcome = write_snapshot(
        &conn,
        prices,
        sealed_prices,
        date,
        identifier,
        lake_ref,
        dry_run,
    );
    match outcome {
        Ok(written) => {
            conn.execute_batch(if dry_run { "ROLLBACK" } else { "COMMIT" })?;
            Ok(written)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

fn write_snapshot(
    conn: &Connection,
    prices: &CardPrices,
    sealed_prices: &SealedPrices,
    date: &str,
    identifier: &str,
    lake_ref: &str,
    dry_run: bool,
) -> Result<usize, Refusal> {
    let staged = stage_prices(conn, prices)?;
    let staged_sealed = stage_sealed_prices(conn, sealed_prices)?;
    conn.execute_batch(
        "DROP TABLE IF EXISTS temp._snap_owned; DROP TABLE IF EXISTS temp._snap_sealed",
    )?;
    conn.execute_batch(&owned_sql())?;
    conn.execute_batch(&owned_sealed_sql())?;
    let copies: i64 = conn.query_row("SELECT COUNT(*) FROM _snap_owned", [], |r| r.get(0))?;
    let (lots, units, unpriced): (i64, i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(quantity), 0), \
                COUNT(*) FILTER (WHERE market_price IS NULL) FROM _snap_sealed",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;

    conn.execute(
        "DELETE FROM collection_value_snapshot WHERE date = ?",
        [date],
    )?;
    let mut written = 0;
    for sql in DIMENSION_SQL {
        written += conn.execute(sql, [date])?;
    }
    conn.execute(
        "INSERT OR REPLACE INTO collection_value_snapshot_run \
            (date, artefact, lake_ref, rows, written_at) VALUES (?, ?, ?, ?, ?)",
        params![
            date,
            identifier,
            lake_ref,
            written as i64,
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
        ],
    )?;
    if dry_run {
        println!(
            "    dry-run: {written} row(s) from {copies} owned cop(ies) in {ZONE_HOLDINGS} and \
             {lots} sealed lot(s) ({units} unit(s)), not written"
        );
        return Ok(written);
    }
    println!(
        "    {written} row(s) from {copies} owned cop(ies) in {ZONE_HOLDINGS} and {lots} sealed \
         lot(s) ({units} unit(s)), {staged} price(s) + {staged_sealed} sealed price(s) staged"
    );
    if unpriced > 0 {
        println!(
            "    {unpriced} owned sealed lot(s) have no price at all and were NOT valued — their \
             units are still counted, never valued at zero"
        );
    }
    Ok(written)
}
