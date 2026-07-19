//! Materialized latest-prices refresh (pokedumpster-vi37).
//!
//! `latest_prices` holds one row per (product, sub_type, source, price_type)
//! at its newest `observed_at`. It used to be a VIEW that `GROUP BY`'d the
//! whole multi-million-row `prices` table; the per-row market-price lookup on
//! the collection/search/binder pages then cost ~1.2s per page load. As an
//! indexed table the same lookup is a point read (~60ms for a full
//! collection).
//!
//! The table + its index are defined once, in `schema_shared.sql` (applied by
//! `open_shared`). This module owns only the *contents*: the table does not
//! auto-reflect `prices`, so it must be rebuilt after prices change. Callers:
//! `pkdump setup`, `pkdump data refresh`, and the UI fixture — all after they
//! finish writing `prices`. It lives in `pkdump-db` (not `pkdump-ingest`) so
//! the db-layer price tests can rebuild it too.
//!
//! Migrating an existing catalog that still has the pre-vi37 VIEW is a
//! one-time manual step on the prod box (`DROP VIEW latest_prices;` then apply
//! the schema table), consistent with this project's manual schema-apply model.

use rusqlite::Connection;

use crate::error::Result;

/// Rebuild the materialized `latest_prices` table from `prices`. Returns the
/// number of rows written. Idempotent: a full replace, not an append. The
/// `BEGIN/COMMIT` keeps the replace atomic so readers never see a half-empty
/// table.
pub fn refresh_latest_prices(conn: &Connection) -> Result<usize> {
    conn.execute_batch(
        "BEGIN IMMEDIATE; \
         DELETE FROM latest_prices; \
         INSERT INTO latest_prices \
             (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) \
         SELECT p.tcgplayer_product_id, p.sub_type_name, p.source, p.price_type, \
                p.price, p.observed_at \
           FROM prices p \
           JOIN (SELECT tcgplayer_product_id, sub_type_name, source, price_type, \
                        MAX(observed_at) AS observed_at \
                   FROM prices GROUP BY 1, 2, 3, 4) m \
             ON p.tcgplayer_product_id = m.tcgplayer_product_id \
            AND p.sub_type_name = m.sub_type_name \
            AND p.source = m.source \
            AND p.price_type = m.price_type \
            AND p.observed_at = m.observed_at; \
         COMMIT;",
    )?;
    let n: i64 = conn.query_row("SELECT count(*) FROM latest_prices", [], |r| r.get(0))?;
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_shared;

    #[test]
    fn materializes_only_the_newest_price_per_key() {
        let dir = tempfile::tempdir().unwrap();
        // open_shared applies schema_shared.sql, creating the latest_prices
        // table + index.
        let conn = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute_batch(
            "INSERT INTO prices (tcgplayer_product_id, sub_type_name, source, price_type, price, observed_at) VALUES \
               (100, 'Normal', 'tcgplayer', 'market', 1.00, '2026-01-01'), \
               (100, 'Normal', 'tcgplayer', 'market', 2.50, '2026-02-01'), \
               (200, 'Holofoil', 'tcgplayer', 'market', 9.00, '2026-01-15');",
        )
        .unwrap();

        let n = refresh_latest_prices(&conn).unwrap();
        assert_eq!(n, 2, "one row per (product,sub_type,source,price_type)");

        // The newest observation wins.
        let price: f64 = conn
            .query_row(
                "SELECT price FROM latest_prices WHERE tcgplayer_product_id = 100 \
                   AND sub_type_name = 'Normal' AND price_type = 'market'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(price, 2.50);

        // Re-running is idempotent (rebuild, not append).
        let n2 = refresh_latest_prices(&conn).unwrap();
        assert_eq!(n2, 2);
        let total: i64 = conn
            .query_row("SELECT count(*) FROM latest_prices", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2);
    }
}
