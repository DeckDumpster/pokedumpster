//! Collection value history (pokedumpster-e1vo).
//!
//! The `collection_value_snapshot` table (schema_user.sql) records the owned
//! collection's total market value, cost basis, and card count on each date,
//! along three dimensions: the whole collection (`all`), per set (`set`), and
//! per binder (`binder`). This module owns the three operations over it:
//!
//! - [`snapshot_today`] computes and upserts *today's* rows.
//! - [`backfill`] reconstructs history from `shared.prices` × each copy's
//!   `acquired_at` / `status_log`, so the chart has a past even though the
//!   snapshot table starts empty.
//!
//! Both price a copy with the **same** three-arm rule, from the one macro in
//! [`crate::prices`] — `latest_prices` → `catalog_price_overrides` →
//! `manual_prices` — differing only in the feed relation and arm 3's cutoff
//! (see [`OWNED_TODAY_SQL`] and [`OWNED_ASOF_SQL`]). They used not to: backfill
//! carried its own query with arm 1 and nothing else, so re-running it silently
//! rewrote every historical point *without* the curated and hand-entered
//! prices today's point has (pd-3lg8).
//! - [`value_history`] reads the table back for the API.
//!
//! VALUE model: for every OWNED copy, `market_price × conditionMultiplier`.
//! The multiplier is data — the collection's own `conditions` table (seeded
//! from `data/conditions.json` on open; pd-s4c2 moved it out of the catalog,
//! so this join no longer crosses the ATTACH boundary), defaulted to `1.0`
//! for an unknown condition, the
//! same defensive default the frontend uses. Cost basis is the sum of owned
//! copies' `purchase_price`; card count is the number of owned copies.

use rusqlite::{Connection, params};

use crate::error::Result;

/// One point on a value series — the collection's value on a single date.
/// FROZEN API contract (pokedumpster-e1vo.1): the chart frontend (e1vo.2) is
/// built against these exact field names.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ValuePoint {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub market_value: f64,
    pub cost_basis: f64,
    #[ts(type = "number")]
    pub card_count: i64,
}

/// A single value line: the whole collection, one set, or one binder.
/// FROZEN API contract (pokedumpster-e1vo.1).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ValueSeries {
    /// `None` for the `all` dimension; otherwise the set_code or the
    /// binder-id-as-string that identifies this bucket.
    pub bucket: Option<String>,
    /// Human label — set name / binder name; `None` for `all`.
    pub label: Option<String>,
    pub points: Vec<ValuePoint>,
}

/// The per-copy "owned collection today" projection: one row per owned copy,
/// carrying its set, binder, purchase price, condition multiplier, and the
/// current market price (latest_prices → manual_prices, same COALESCE as the
/// collection page). Built into a TEMP TABLE so the three per-dimension
/// aggregates each scan it once.
const OWNED_TODAY_SQL: &str = concat!(
    "\
    CREATE TEMP TABLE _snap_owned AS \
    SELECT c.id, \
           c.purchase_price, \
           c.binder_id, \
           cd.set_code, \
           COALESCE(cond.multiplier, 1.0) AS mult, \
           ",
    crate::market_price_expr!(),
    " AS market_price \
      FROM collection c \
      JOIN ( \
             SELECT printing_id, card_id, tcgplayer_product_id, sub_type_name \
               FROM printings \
             UNION ALL \
             SELECT printing_id, card_id, NULL, NULL \
               FROM user_printings \
           ) p ON c.printing_id = p.printing_id \
      JOIN cards cd ON p.card_id = cd.card_id \
      LEFT JOIN conditions cond ON cond.name = c.condition \
     WHERE c.status = 'owned';"
);

/// The same per-copy projection as [`OWNED_TODAY_SQL`], for the collection as
/// it stood on a date D bound to `?1`: the copies owned on D, at the price
/// they carried on D. `_prices_asof` (staged per date by
/// [`backfill_one_date`]) stands in for `latest_prices`, and arm 3 is cut off
/// at D — every other arm is the *same text*, because it is the same macro.
const OWNED_ASOF_SQL: &str = concat!(
    "\
    CREATE TEMP TABLE _snap_owned AS \
    SELECT c.id, \
           c.purchase_price, \
           c.binder_id, \
           cd.set_code, \
           COALESCE(cond.multiplier, 1.0) AS mult, \
           ",
    crate::market_price_expr_asof!("?1"),
    " AS market_price \
      FROM collection c \
      JOIN ( \
             SELECT printing_id, card_id, tcgplayer_product_id, sub_type_name \
               FROM printings \
             UNION ALL \
             SELECT printing_id, card_id, NULL, NULL \
               FROM user_printings \
           ) p ON c.printing_id = p.printing_id \
      JOIN cards cd ON p.card_id = cd.card_id \
      LEFT JOIN conditions cond ON cond.name = c.condition \
     WHERE date(c.acquired_at) <= ?1 \
       AND COALESCE( \
             (SELECT sl.to_status FROM status_log sl \
               WHERE sl.collection_id = c.id \
                 AND date(sl.changed_at) <= ?1 \
               ORDER BY sl.changed_at DESC, sl.id DESC LIMIT 1), \
             'owned') = 'owned';"
);

/// Compute and upsert today's value rows for all three dimensions. `date` is
/// the `YYYY-MM-DD` snapshot key (the caller passes today). Idempotent: a
/// full delete-then-insert per `(date, dimension)`, so re-running replaces the
/// day's rows rather than duplicating them (a plain upsert can't, because the
/// `all` bucket is NULL and SQLite treats NULLs as distinct in the PK).
///
/// Returns the number of snapshot rows written.
pub fn snapshot_today(conn: &mut Connection, date: &str) -> Result<usize> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM collection_value_snapshot WHERE date = ?1",
        params![date],
    )?;
    tx.execute_batch("DROP TABLE IF EXISTS _snap_owned;")?;
    tx.execute_batch(OWNED_TODAY_SQL)?;

    let written = insert_dimensions(&tx, date)?;

    tx.execute_batch("DROP TABLE IF EXISTS _snap_owned;")?;
    tx.commit()?;
    Ok(written)
}

/// Insert the `all` / `set` / `binder` aggregate rows for `date` from the
/// current `_snap_owned` TEMP TABLE. Shared by [`snapshot_today`] and the
/// per-date step of [`backfill`], which both stage owned copies (with a
/// `market_price` + `mult` column) into `_snap_owned` first.
fn insert_dimensions(tx: &rusqlite::Transaction, date: &str) -> Result<usize> {
    let mut written = 0usize;
    written += tx.execute(
        "INSERT INTO collection_value_snapshot \
             (date, dimension, bucket, market_value, cost_basis, card_count) \
         SELECT ?1, 'all', NULL, \
                COALESCE(SUM(market_price * mult), 0.0), \
                COALESCE(SUM(purchase_price), 0.0), \
                COUNT(*) \
           FROM _snap_owned",
        params![date],
    )?;
    written += tx.execute(
        "INSERT INTO collection_value_snapshot \
             (date, dimension, bucket, market_value, cost_basis, card_count) \
         SELECT ?1, 'set', set_code, \
                COALESCE(SUM(market_price * mult), 0.0), \
                COALESCE(SUM(purchase_price), 0.0), \
                COUNT(*) \
           FROM _snap_owned \
          GROUP BY set_code",
        params![date],
    )?;
    written += tx.execute(
        "INSERT INTO collection_value_snapshot \
             (date, dimension, bucket, market_value, cost_basis, card_count) \
         SELECT ?1, 'binder', CAST(binder_id AS TEXT), \
                COALESCE(SUM(market_price * mult), 0.0), \
                COALESCE(SUM(purchase_price), 0.0), \
                COUNT(*) \
           FROM _snap_owned \
          WHERE binder_id IS NOT NULL \
          GROUP BY binder_id",
        params![date],
    )?;
    Ok(written)
}

/// Reconstruct historical value rows from `shared.prices` and each copy's
/// acquisition + status history. For every distinct market-price observation
/// date D at or after the earliest acquisition, records the value of the
/// collection as it stood on D:
///
/// - **owned on D**: a copy with `date(acquired_at) <= D` whose status as of D
///   is `owned`. Status-as-of-D is the `to_status` of the latest `status_log`
///   transition with `date(changed_at) <= D`; if there is none (e.g. the only
///   log row is a backdated acquisition recorded later), the copy is treated
///   as owned since acquisition.
/// - **price as of D**: [`crate::prices`]'s three arms, resolved at D — the
///   latest `prices` row (`price_type='market'`) with `observed_at <= D` for
///   the copy's product+sub_type, then `catalog_price_overrides`, then the
///   tenant's own `manual_prices` observed on or before D. Arm 1 is the only
///   one whose *shape* changes with the date; the expression itself is the
///   same macro `snapshot_today` spends, so the two paths cannot drift
///   (pd-3lg8).
/// - **condition**: the copy's *current* condition — the accepted
///   approximation, since there is no condition history.
///
/// Idempotent: each date's rows are deleted before being re-inserted.
///
/// PERF: the trap is O(dates × copies × price-lookup). We avoid it two ways —
/// (1) a `_owned_products` TEMP TABLE (built once) restricts the per-date
/// price materialization to the handful of products the collection has ever
/// held, so `_prices_asof` is a small join, not a scan of the whole
/// multi-million-row `prices` table; (2) `_prices_asof` is built once per date
/// and joined to owned copies, never a correlated per-copy subquery. Each date
/// is its own transaction (interrupt-safe) with a flushed progress line.
///
/// Returns the total number of snapshot rows written.
pub fn backfill(conn: &mut Connection) -> Result<usize> {
    use std::io::Write;

    // Products (and sub_types) the collection has ever held. Restricting the
    // per-date price materialization to these is what keeps backfill cheap.
    conn.execute_batch(
        "DROP TABLE IF EXISTS _owned_products; \
         CREATE TEMP TABLE _owned_products AS \
         SELECT DISTINCT p.tcgplayer_product_id, p.sub_type_name \
           FROM collection c \
           JOIN printings p ON c.printing_id = p.printing_id \
          WHERE p.tcgplayer_product_id IS NOT NULL; \
         CREATE INDEX _owned_products_idx \
           ON _owned_products(tcgplayer_product_id, sub_type_name);",
    )?;

    // Earliest acquisition date — no point recording dates before the
    // collection had a single card (they would all be value 0).
    let earliest: Option<String> =
        conn.query_row("SELECT MIN(date(acquired_at)) FROM collection", [], |r| {
            r.get(0)
        })?;
    let Some(earliest) = earliest else {
        println!("Backfill: collection is empty — nothing to reconstruct.");
        return Ok(0);
    };

    // Distinct market-price observation dates at or after the first
    // acquisition, oldest first.
    let dates: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT observed_at FROM prices \
              WHERE price_type = 'market' AND observed_at >= ?1 \
              ORDER BY observed_at",
        )?;
        let rows = stmt.query_map(params![earliest], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    println!(
        "Backfill: {} distinct price date(s) from {} onward.",
        dates.len(),
        earliest
    );
    std::io::stdout().flush().ok();

    let mut written = 0usize;
    for (i, date) in dates.iter().enumerate() {
        written += backfill_one_date(conn, date)?;
        if (i + 1) % 25 == 0 || i + 1 == dates.len() {
            println!(
                "  {}/{} dates ({} rows so far)",
                i + 1,
                dates.len(),
                written
            );
            std::io::stdout().flush().ok();
        }
    }

    conn.execute_batch("DROP TABLE IF EXISTS _owned_products;")?;
    println!(
        "Backfill: wrote {written} snapshot rows across {} dates.",
        dates.len()
    );
    Ok(written)
}

/// Reconstruct and write the three dimension rows for one date D. Assumes the
/// `_owned_products` TEMP TABLE already exists (built once by [`backfill`]).
fn backfill_one_date(conn: &mut Connection, date: &str) -> Result<usize> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM collection_value_snapshot WHERE date = ?1",
        params![date],
    )?;

    // Latest market price <= D per owned product+sub_type. Built once per
    // date; the owned-products restriction keeps it small.
    tx.execute_batch("DROP TABLE IF EXISTS _prices_asof; DROP TABLE IF EXISTS _snap_owned;")?;
    tx.execute(
        "CREATE TEMP TABLE _prices_asof AS \
         SELECT pr.tcgplayer_product_id, pr.sub_type_name, pr.price_type, pr.price \
           FROM prices pr \
           JOIN _owned_products op \
             ON op.tcgplayer_product_id = pr.tcgplayer_product_id \
            AND op.sub_type_name = pr.sub_type_name \
           JOIN ( \
                  SELECT tcgplayer_product_id, sub_type_name, MAX(observed_at) AS mo \
                    FROM prices \
                   WHERE price_type = 'market' AND observed_at <= ?1 \
                     AND tcgplayer_product_id IN \
                         (SELECT tcgplayer_product_id FROM _owned_products) \
                   GROUP BY tcgplayer_product_id, sub_type_name \
                ) m \
             ON m.tcgplayer_product_id = pr.tcgplayer_product_id \
            AND m.sub_type_name = pr.sub_type_name \
            AND m.mo = pr.observed_at \
          WHERE pr.price_type = 'market'",
        params![date],
    )?;
    tx.execute_batch(
        "CREATE INDEX _prices_asof_idx \
           ON _prices_asof(tcgplayer_product_id, sub_type_name);",
    )?;

    // Copies owned on D, priced as of D. Same _snap_owned shape
    // insert_dimensions expects (market_price + mult columns), and the same
    // three-arm rule snapshot_today spends — spelled once, in
    // `market_price_expr_from!`, with `_prices_asof` standing in for
    // `latest_prices` and arm 3 cut off at D.
    tx.execute(OWNED_ASOF_SQL, params![date])?;

    let written = insert_dimensions(&tx, date)?;

    tx.execute_batch("DROP TABLE IF EXISTS _prices_asof; DROP TABLE IF EXISTS _snap_owned;")?;
    tx.commit()?;
    Ok(written)
}

/// Read the value history for the API. `dimension` is `all`, `set`, or
/// `binder` (an unknown value is the caller's responsibility to default).
///
/// - `all` → exactly one series (`bucket`/`label` both `None`), points sorted
///   by date ascending.
/// - `set` / `binder` → one series per bucket, points sorted by date; series
///   sorted by their latest (most recent date) market value, descending. The
///   label is the set name / binder name.
pub fn value_history(conn: &Connection, dimension: &str) -> Result<Vec<ValueSeries>> {
    if dimension == "all" {
        let mut stmt = conn.prepare(
            "SELECT date, market_value, cost_basis, card_count \
               FROM collection_value_snapshot \
              WHERE dimension = 'all' \
              ORDER BY date",
        )?;
        let points: Vec<ValuePoint> = stmt
            .query_map([], |r| {
                Ok(ValuePoint {
                    date: r.get(0)?,
                    market_value: r.get(1)?,
                    cost_basis: r.get(2)?,
                    card_count: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        // Always return exactly one series for 'all', even when empty.
        return Ok(vec![ValueSeries {
            bucket: None,
            label: None,
            points,
        }]);
    }

    // set / binder: bucket + label come from the catalog / user tables.
    let sql = match dimension {
        "binder" => {
            "SELECT cvs.bucket, b.name, cvs.date, \
                    cvs.market_value, cvs.cost_basis, cvs.card_count \
               FROM collection_value_snapshot cvs \
               LEFT JOIN binders b ON b.id = CAST(cvs.bucket AS INTEGER) \
              WHERE cvs.dimension = 'binder' \
              ORDER BY cvs.bucket, cvs.date"
        }
        // 'set' (and any caller-normalized default that reaches here).
        _ => {
            "SELECT cvs.bucket, s.name, cvs.date, \
                    cvs.market_value, cvs.cost_basis, cvs.card_count \
               FROM collection_value_snapshot cvs \
               LEFT JOIN sets s ON s.set_code = cvs.bucket \
              WHERE cvs.dimension = 'set' \
              ORDER BY cvs.bucket, cvs.date"
        }
    };

    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([], |r| {
        let bucket: Option<String> = r.get(0)?;
        let label: Option<String> = r.get(1)?;
        let point = ValuePoint {
            date: r.get(2)?,
            market_value: r.get(3)?,
            cost_basis: r.get(4)?,
            card_count: r.get(5)?,
        };
        Ok((bucket, label, point))
    })?;

    // Group consecutive rows by bucket (the query is ordered by bucket, date).
    let mut series: Vec<ValueSeries> = Vec::new();
    for row in rows {
        let (bucket, label, point) = row?;
        match series.last_mut() {
            Some(s) if s.bucket == bucket => s.points.push(point),
            _ => series.push(ValueSeries {
                bucket,
                label,
                points: vec![point],
            }),
        }
    }

    // Sort series by their latest (most recent date) market value, desc. The
    // points within each series are already date-ascending, so the last point
    // is the latest.
    series.sort_by(|a, b| {
        let av = a.points.last().map(|p| p.market_value).unwrap_or(0.0);
        let bv = b.points.last().map(|p| p.market_value).unwrap_or(0.0);
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(series)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::latest_prices::refresh_latest_prices;
    use crate::{connect_user, open_shared};
    use rusqlite::Connection;

    /// Build a shared catalog with one set, two cards, and priced printings,
    /// then return a user connection attached to it.
    ///
    /// Catalog:
    ///   set 'set1' "First Set"
    ///   card set1-1 "Alpha"  → printing set1-1-normal  (product 100, 'Normal')
    ///   card set1-2 "Beta"   → printing set1-2-holo    (product 200, 'Holofoil')
    fn fixture() -> (tempfile::TempDir, Connection, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('set1', 'First Set', 'Test')",
                [],
            )
            .unwrap();
            for (id, num, name) in [("set1-1", "1", "Alpha"), ("set1-2", "2", "Beta")] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, 'set1', ?2, ?3, ?4)",
                    params![id, num, num.parse::<i64>().unwrap(), name],
                )
                .unwrap();
            }
            for (pid, card, variant, product, sub) in [
                ("set1-1-normal", "set1-1", "normal", 100i64, "Normal"),
                ("set1-2-holo", "set1-2", "holo", 200i64, "Holofoil"),
            ] {
                c.execute(
                    "INSERT INTO printings \
                       (printing_id, card_id, variant, tcgplayer_product_id, sub_type_name) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![pid, card, variant, product, sub],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn, shared)
    }

    fn add_copy(conn: &mut Connection, printing: &str, condition: &str, price: f64) -> i64 {
        conn.execute(
            "INSERT INTO collection (printing_id, condition, purchase_price, acquired_at, source, status) \
             VALUES (?1, ?2, ?3, ?4, 'manual_id', 'owned')",
            params![printing, condition, price, "2026-01-01T00:00:00Z"],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO status_log (collection_id, from_status, to_status, changed_at, note) \
             VALUES (?1, NULL, 'owned', ?2, 'added')",
            params![id, "2026-01-01T00:00:00Z"],
        )
        .unwrap();
        id
    }

    /// Append a market-price observation. `prices` and `latest_prices` are
    /// shared-catalog tables (read-only views on the user connection), so the
    /// write goes through a fresh read-write handle on the shared DB. The
    /// user connection reads the committed rows back through its attachment.
    fn set_price(shared: &std::path::Path, product: i64, sub: &str, price: f64, observed: &str) {
        let c = open_shared(shared).unwrap();
        c.execute(
            "INSERT INTO prices \
               (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
             VALUES (?1, ?2, 'tcgplayer', 'market', ?3, ?4)",
            params![product, sub, price, observed],
        )
        .unwrap();
    }

    /// Rebuild the materialized `latest_prices` table on the shared DB — the
    /// source `snapshot_today` reads for today's market price.
    fn refresh_shared_latest(shared: &std::path::Path) {
        let c = open_shared(shared).unwrap();
        refresh_latest_prices(&c).unwrap();
    }

    fn point_for(
        conn: &Connection,
        dimension: &str,
        bucket: Option<&str>,
        date: &str,
    ) -> ValuePoint {
        let series = value_history(conn, dimension).unwrap();
        let s = series
            .iter()
            .find(|s| s.bucket.as_deref() == bucket)
            .unwrap_or_else(|| panic!("no series for bucket {bucket:?}"));
        s.points
            .iter()
            .find(|p| p.date == date)
            .unwrap_or_else(|| panic!("no point on {date}"))
            .clone()
    }

    #[test]
    fn snapshot_today_computes_value_cost_and_count() {
        let (_d, mut conn, shared) = fixture();
        // Alpha (Normal, product 100) market 10.00; Beta (Holo, 200) market 40.00.
        set_price(&shared, 100, "Normal", 10.00, "2026-06-01");
        set_price(&shared, 200, "Holofoil", 40.00, "2026-06-01");
        refresh_shared_latest(&shared);

        // Two NM Alphas (10.00 each), one Lightly Played Beta (40 * 0.85 = 34).
        add_copy(&mut conn, "set1-1-normal", "Near Mint", 3.00);
        add_copy(&mut conn, "set1-1-normal", "Near Mint", 4.00);
        add_copy(&mut conn, "set1-2-holo", "Lightly Played", 20.00);

        let n = snapshot_today(&mut conn, "2026-06-02").unwrap();
        // 1 'all' + 1 'set' bucket (both cards live in set1) + 0 'binder' = 2 rows.
        assert_eq!(n, 2);

        let all = point_for(&conn, "all", None, "2026-06-02");
        // 10 + 10 + 34 = 54.00
        assert!(
            (all.market_value - 54.00).abs() < 1e-9,
            "value {}",
            all.market_value
        );
        // cost basis 3 + 4 + 20 = 27
        assert!(
            (all.cost_basis - 27.00).abs() < 1e-9,
            "cost {}",
            all.cost_basis
        );
        assert_eq!(all.card_count, 3);

        // Per-set: set1 holds all three copies.
        let set1 = point_for(&conn, "set", Some("set1"), "2026-06-02");
        assert!((set1.market_value - 54.00).abs() < 1e-9);
        assert_eq!(set1.card_count, 3);
    }

    #[test]
    fn snapshot_today_is_idempotent() {
        let (_d, mut conn, shared) = fixture();
        set_price(&shared, 100, "Normal", 10.00, "2026-06-01");
        refresh_shared_latest(&shared);
        add_copy(&mut conn, "set1-1-normal", "Near Mint", 1.00);

        snapshot_today(&mut conn, "2026-06-02").unwrap();
        snapshot_today(&mut conn, "2026-06-02").unwrap();
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM collection_value_snapshot WHERE date = '2026-06-02'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 2, "re-run replaces, not duplicates (1 all + 1 set)");
    }

    #[test]
    fn snapshot_excludes_non_owned_copies() {
        let (_d, mut conn, shared) = fixture();
        set_price(&shared, 100, "Normal", 10.00, "2026-06-01");
        refresh_shared_latest(&shared);
        let sold = add_copy(&mut conn, "set1-1-normal", "Near Mint", 1.00);
        add_copy(&mut conn, "set1-1-normal", "Near Mint", 1.00);
        conn.execute(
            "UPDATE collection SET status = 'sold' WHERE id = ?1",
            params![sold],
        )
        .unwrap();

        snapshot_today(&mut conn, "2026-06-02").unwrap();
        let all = point_for(&conn, "all", None, "2026-06-02");
        assert_eq!(all.card_count, 1, "only the owned copy counts");
        assert!((all.market_value - 10.00).abs() < 1e-9);
    }

    #[test]
    fn snapshot_groups_by_binder() {
        let (_d, mut conn, shared) = fixture();
        set_price(&shared, 100, "Normal", 10.00, "2026-06-01");
        set_price(&shared, 200, "Holofoil", 40.00, "2026-06-01");
        refresh_shared_latest(&shared);
        conn.execute(
            "INSERT INTO binders (id, name, created_at, updated_at) \
             VALUES (7, 'Fav', '2026-01-01', '2026-01-01')",
            [],
        )
        .unwrap();
        let a = add_copy(&mut conn, "set1-1-normal", "Near Mint", 1.00);
        add_copy(&mut conn, "set1-2-holo", "Near Mint", 1.00); // unassigned
        conn.execute(
            "UPDATE collection SET binder_id = 7 WHERE id = ?1",
            params![a],
        )
        .unwrap();

        snapshot_today(&mut conn, "2026-06-02").unwrap();
        let series = value_history(&conn, "binder").unwrap();
        assert_eq!(series.len(), 1, "only the assigned binder gets a bucket");
        assert_eq!(series[0].bucket.as_deref(), Some("7"));
        assert_eq!(series[0].label.as_deref(), Some("Fav"));
        assert!((series[0].points[0].market_value - 10.00).abs() < 1e-9);
    }

    #[test]
    fn backfill_reconstructs_value_over_dates() {
        let (_d, mut conn, shared) = fixture();

        // Alpha bought 2026-03-01, Beta bought 2026-05-01 (backdated acquisitions).
        conn.execute(
            "INSERT INTO collection (printing_id, condition, purchase_price, acquired_at, source, status) \
             VALUES ('set1-1-normal', 'Near Mint', 2.00, '2026-03-01T00:00:00Z', 'manual_id', 'owned')",
            [],
        )
        .unwrap();
        let alpha = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO status_log (collection_id, from_status, to_status, changed_at, note) \
             VALUES (?1, NULL, 'owned', '2026-06-30T00:00:00Z', 'added')",
            params![alpha],
        )
        .unwrap(); // status_log recorded LATE (after the price dates) — backfill must
        // fall back to 'owned since acquired_at'.
        conn.execute(
            "INSERT INTO collection (printing_id, condition, purchase_price, acquired_at, source, status) \
             VALUES ('set1-2-holo', 'Near Mint', 30.00, '2026-05-01T00:00:00Z', 'manual_id', 'owned')",
            [],
        )
        .unwrap();
        let beta = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO status_log (collection_id, from_status, to_status, changed_at, note) \
             VALUES (?1, NULL, 'owned', '2026-06-30T00:00:00Z', 'added')",
            params![beta],
        )
        .unwrap();

        // Prices: Alpha rises 10 → 15; Beta priced at 40 from April.
        set_price(&shared, 100, "Normal", 10.00, "2026-04-01");
        set_price(&shared, 100, "Normal", 15.00, "2026-05-15");
        set_price(&shared, 200, "Holofoil", 40.00, "2026-04-01");

        let n = backfill(&mut conn).unwrap();
        assert!(n > 0);

        // 2026-04-01: only Alpha owned (Beta acquired 05-01). Value = 10.
        let d1 = point_for(&conn, "all", None, "2026-04-01");
        assert_eq!(d1.card_count, 1, "only Alpha owned on 04-01");
        assert!(
            (d1.market_value - 10.00).abs() < 1e-9,
            "value {}",
            d1.market_value
        );

        // 2026-05-15: Alpha (now 15) + Beta (40, latest <=D is the 04-01 obs) = 55.
        let d2 = point_for(&conn, "all", None, "2026-05-15");
        assert_eq!(d2.card_count, 2, "both owned by 05-15");
        assert!(
            (d2.market_value - 55.00).abs() < 1e-9,
            "value {}",
            d2.market_value
        );
        // cost basis grows to 2 + 30 = 32.
        assert!((d2.cost_basis - 32.00).abs() < 1e-9);
    }

    #[test]
    fn backfill_respects_sold_status_from_log() {
        let (_d, mut conn, shared) = fixture();
        conn.execute(
            "INSERT INTO collection (printing_id, condition, purchase_price, acquired_at, source, status) \
             VALUES ('set1-1-normal', 'Near Mint', 2.00, '2026-03-01T00:00:00Z', 'manual_id', 'sold')",
            [],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        // Owned from acquisition, then sold on 2026-05-01.
        conn.execute(
            "INSERT INTO status_log (collection_id, from_status, to_status, changed_at, note) \
             VALUES (?1, NULL, 'owned', '2026-03-01T00:00:00Z', 'added'), \
                    (?1, 'owned', 'sold', '2026-05-01T00:00:00Z', 'ebay')",
            params![id],
        )
        .unwrap();
        set_price(&shared, 100, "Normal", 10.00, "2026-04-01");
        set_price(&shared, 100, "Normal", 12.00, "2026-06-01");

        backfill(&mut conn).unwrap();

        // 04-01: owned → counted.
        let d1 = point_for(&conn, "all", None, "2026-04-01");
        assert_eq!(d1.card_count, 1);
        assert!((d1.market_value - 10.00).abs() < 1e-9);

        // 06-01: sold before this date → not counted.
        let d2 = point_for(&conn, "all", None, "2026-06-01");
        assert_eq!(d2.card_count, 0, "sold copy drops out after the sale date");
        assert!((d2.market_value - 0.00).abs() < 1e-9);
    }

    /// Every snapshot row for `date`, in a stable order — what the two paths
    /// must agree on, column for column.
    fn rows_on(
        conn: &Connection,
        date: &str,
    ) -> Vec<(String, String, Option<String>, f64, f64, i64)> {
        let mut stmt = conn
            .prepare(
                "SELECT date, dimension, bucket, market_value, cost_basis, card_count \
                   FROM collection_value_snapshot \
                  WHERE date = ?1 \
                  ORDER BY dimension, bucket",
            )
            .unwrap();
        stmt.query_map(params![date], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
    }

    /// pd-3lg8: the live path and the backfill must be the SAME arithmetic.
    ///
    /// The collection holds one copy priced by each of the three arms — the
    /// TCGplayer feed, a curated `catalog_price_overrides` row for a catalog
    /// printing the feed does not price, and a `manual_prices` row for a
    /// printing this tenant invented. `snapshot_today` for D and `backfill`
    /// ending on D must write byte-identical rows for D.
    ///
    /// Before the fix backfill resolved arm 1 alone, so it wrote a strictly
    /// smaller number for the same day — which is what re-running it against
    /// prod did to 60 dates.
    #[test]
    fn snapshot_today_and_backfill_agree_on_the_same_date() {
        let (_d, mut conn, shared) = fixture();

        // Arm 2's subject: a catalog printing the feed does not price, with a
        // curated override. Both live in `shared`.
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('set1-3', 'set1', '3', 3, 'Gamma')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('set1-3-normal', 'set1-3', 'normal')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO catalog_price_overrides (printing_id, price, observed_at, note) \
                 VALUES ('set1-3-normal', 61.00, '2026-05-01', 'feed has no product')",
                [],
            )
            .unwrap();
        }

        // Arm 3's subject: a printing this tenant invented, hand-priced.
        conn.execute(
            "INSERT INTO user_printings (printing_id, card_id, variant, created_at) \
             VALUES ('set1-1-user-1', 'set1-1', 'invented', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO manual_prices (printing_id, price, observed_at) \
             VALUES ('set1-1-user-1', 7.50, '2026-05-20T00:00:00Z')",
            [],
        )
        .unwrap();

        // Arm 1's subject, observed on the very day we snapshot — which is
        // also what puts D in backfill's date list.
        set_price(&shared, 100, "Normal", 10.00, "2026-06-02");
        refresh_shared_latest(&shared);

        add_copy(&mut conn, "set1-1-normal", "Near Mint", 3.00);
        add_copy(&mut conn, "set1-3-normal", "Lightly Played", 40.00);
        add_copy(&mut conn, "set1-1-user-1", "Near Mint", 2.00);

        snapshot_today(&mut conn, "2026-06-02").unwrap();
        let live = rows_on(&conn, "2026-06-02");

        // 10.00 + 61.00 * 0.85 + 7.50 = 69.35 — every arm contributing.
        assert_eq!(live.len(), 2, "1 'all' + 1 'set' row");
        assert!(
            (live[0].3 - 69.35).abs() < 1e-9,
            "live 'all' value {} — all three arms must contribute",
            live[0].3
        );

        backfill(&mut conn).unwrap();
        let reconstructed = rows_on(&conn, "2026-06-02");

        assert_eq!(
            reconstructed, live,
            "backfill must reconstruct the same day the live path computes"
        );
    }

    #[test]
    fn value_history_all_returns_single_series_even_when_empty() {
        let (_d, conn, _shared) = fixture();
        let series = value_history(&conn, "all").unwrap();
        assert_eq!(series.len(), 1);
        assert!(series[0].bucket.is_none());
        assert!(series[0].label.is_none());
        assert!(series[0].points.is_empty());
    }

    #[test]
    fn value_history_sets_sorted_by_latest_value_desc() {
        // Two sets; the more valuable one must come first.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            for (code, name) in [("cheap", "Cheap Set"), ("pricey", "Pricey Set")] {
                c.execute(
                    "INSERT INTO sets (set_code, name, series) VALUES (?1, ?2, 'Test')",
                    params![code, name],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('cheap-1', 'cheap', '1', 1, 'C'), ('pricey-1', 'pricey', '1', 1, 'P')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id, sub_type_name) \
                 VALUES ('cheap-1-n', 'cheap-1', 'normal', 1, 'Normal'), \
                        ('pricey-1-n', 'pricey-1', 'normal', 2, 'Normal')",
                [],
            )
            .unwrap();
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        set_price(&shared, 1, "Normal", 5.00, "2026-06-01");
        set_price(&shared, 2, "Normal", 99.00, "2026-06-01");
        refresh_shared_latest(&shared);
        add_copy(&mut conn, "cheap-1-n", "Near Mint", 1.00);
        add_copy(&mut conn, "pricey-1-n", "Near Mint", 1.00);

        snapshot_today(&mut conn, "2026-06-02").unwrap();
        let series = value_history(&conn, "set").unwrap();
        assert_eq!(series.len(), 2);
        assert_eq!(
            series[0].bucket.as_deref(),
            Some("pricey"),
            "pricier set first"
        );
        assert_eq!(series[0].label.as_deref(), Some("Pricey Set"));
        assert_eq!(series[1].bucket.as_deref(), Some("cheap"));
    }
}
