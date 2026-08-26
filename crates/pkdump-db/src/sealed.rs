//! Repository for the sealed-product collection (PLAN.md §8): the user's
//! booster boxes, ETBs, bundles, and tins, plus catalog search to add them.

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog;
use crate::error::{DbError, Result};

/// A sealed product from the catalog — used when searching for something to
/// add to the collection.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SealedProduct {
    #[ts(type = "number")]
    pub product_id: i64,
    pub set_code: Option<String>,
    pub name: String,
    pub category: String,
    pub image_url: Option<String>,
    pub release_date: Option<String>,
}

/// A sealed-collection row joined to its catalog product.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SealedEntry {
    #[ts(type = "number")]
    pub id: i64,
    #[ts(type = "number")]
    pub product_id: i64,
    #[ts(type = "number")]
    pub quantity: i64,
    pub condition: Option<String>,
    pub purchase_price: Option<f64>,
    pub sale_price: Option<f64>,
    pub purchase_date: Option<String>,
    pub source: Option<String>,
    pub seller_name: Option<String>,
    pub notes: Option<String>,
    pub status: String,
    pub added_at: String,
    pub name: String,
    pub category: String,
    pub set_code: Option<String>,
    pub image_url: Option<String>,
    /// Latest TCGplayer sealed market price for this product (NULL when no
    /// snapshot exists). Sealed products are keyed by TCGplayer productId, so
    /// the join is `latest_sealed_prices.tcgplayer_product_id =
    /// sealed_collection.product_id`. Falls back to the day's mid price when
    /// TCGCSV omits a market price for the product (common for sealed).
    pub market_price: Option<f64>,
}

/// Fields for adding a sealed product to the collection.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewSealed {
    #[ts(type = "number")]
    pub product_id: i64,
    #[ts(type = "number | null")]
    pub quantity: Option<i64>,
    pub condition: Option<String>,
    pub purchase_price: Option<f64>,
    pub purchase_date: Option<String>,
    pub source: Option<String>,
    pub seller_name: Option<String>,
    pub notes: Option<String>,
}

/// Editable sealed-entry fields, including `status` for disposal. A `None`
/// field is left unchanged.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct SealedEdit {
    #[ts(type = "number | null")]
    pub quantity: Option<i64>,
    pub condition: Option<String>,
    pub purchase_price: Option<f64>,
    pub sale_price: Option<f64>,
    pub purchase_date: Option<String>,
    pub source: Option<String>,
    pub seller_name: Option<String>,
    pub notes: Option<String>,
    pub status: Option<String>,
}

const ENTRY_COLS: &str = concat!(
    "sc.id, sc.product_id, sc.quantity, sc.condition, \
     sc.purchase_price, sc.sale_price, sc.purchase_date, sc.source, \
     sc.seller_name, sc.notes, sc.status, sc.added_at, \
     sp.name, sp.category, sp.set_code, sp.image_url, ",
    // The ONE spelling of what a sealed lot is worth (`crate::prices`). The
    // value chart's `sealed` dimension spends the same macro, so a box on
    // this page and the same box on the chart cannot be priced by two rules.
    crate::sealed_market_price_expr!(),
    " AS market_price"
);

const ENTRY_FROM: &str = "FROM sealed_collection sc \
     JOIN sealed_products sp ON sc.product_id = sp.product_id";

fn entry_from_row(r: &rusqlite::Row) -> rusqlite::Result<SealedEntry> {
    Ok(SealedEntry {
        id: r.get(0)?,
        product_id: r.get(1)?,
        quantity: r.get(2)?,
        condition: r.get(3)?,
        purchase_price: r.get(4)?,
        sale_price: r.get(5)?,
        purchase_date: r.get(6)?,
        source: r.get(7)?,
        seller_name: r.get(8)?,
        notes: r.get(9)?,
        status: r.get(10)?,
        added_at: r.get(11)?,
        name: r.get(12)?,
        category: r.get(13)?,
        set_code: r.get(14)?,
        image_url: r.get(15)?,
        market_price: r.get(16)?,
    })
}

/// Search the sealed-product catalog by name.
pub fn search_products(conn: &Connection, query: &str, limit: i64) -> Result<Vec<SealedProduct>> {
    let mut stmt = conn.prepare(
        "SELECT product_id, set_code, name, category, image_url, release_date \
         FROM sealed_products WHERE name LIKE ?1 ORDER BY name LIMIT ?2",
    )?;
    let like = format!("%{query}%");
    let rows = stmt.query_map(params![like, limit], |r| {
        Ok(SealedProduct {
            product_id: r.get(0)?,
            set_code: r.get(1)?,
            name: r.get(2)?,
            category: r.get(3)?,
            image_url: r.get(4)?,
            release_date: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// List the user's sealed collection, newest first.
pub fn list(conn: &Connection) -> Result<Vec<SealedEntry>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ENTRY_COLS} {ENTRY_FROM} ORDER BY sc.id DESC"
    ))?;
    let rows = stmt.query_map([], entry_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Fetch one sealed entry.
pub fn get(conn: &Connection, id: i64) -> Result<Option<SealedEntry>> {
    Ok(conn
        .prepare(&format!(
            "SELECT {ENTRY_COLS} {ENTRY_FROM} WHERE sc.id = ?1"
        ))?
        .query_row([id], entry_from_row)
        .optional()?)
}

/// Add a sealed product to the collection. Validates the product against the
/// catalog. Returns the new row id.
pub fn add(conn: &Connection, new: &NewSealed) -> Result<i64> {
    if !catalog::sealed_product_exists(conn, new.product_id)? {
        return Err(DbError::NotFound(format!(
            "sealed product {} is not in the catalog",
            new.product_id
        )));
    }
    conn.execute(
        "INSERT INTO sealed_collection \
           (product_id, quantity, condition, purchase_price, purchase_date, \
            source, seller_name, notes, status, added_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'owned', ?9)",
        params![
            new.product_id,
            new.quantity.unwrap_or(1),
            new.condition,
            new.purchase_price,
            new.purchase_date,
            new.source,
            new.seller_name,
            new.notes,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Update editable fields (including `status`, for disposal). Returns
/// whether a row changed.
pub fn update(conn: &Connection, id: i64, edit: &SealedEdit) -> Result<bool> {
    let n = conn.execute(
        "UPDATE sealed_collection SET \
           quantity       = COALESCE(?2, quantity), \
           condition      = COALESCE(?3, condition), \
           purchase_price = COALESCE(?4, purchase_price), \
           sale_price     = COALESCE(?5, sale_price), \
           purchase_date  = COALESCE(?6, purchase_date), \
           source         = COALESCE(?7, source), \
           seller_name    = COALESCE(?8, seller_name), \
           notes          = COALESCE(?9, notes), \
           status         = COALESCE(?10, status) \
         WHERE id = ?1",
        params![
            id,
            edit.quantity,
            edit.condition,
            edit.purchase_price,
            edit.sale_price,
            edit.purchase_date,
            edit.source,
            edit.seller_name,
            edit.notes,
            edit.status,
        ],
    )?;
    Ok(n > 0)
}

/// Delete a sealed-collection entry.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM sealed_collection WHERE id = ?1", [id])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sealed_products (product_id, name, category, fetched_at) \
                 VALUES (5001, '151 Elite Trainer Box', 'elite_trainer_box', '2026-05-18')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn add_list_update_dispose_delete() {
        let (_d, conn) = conn();

        // Catalog search finds the product.
        let found = search_products(&conn, "Elite", 10).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].product_id, 5001);

        let id = add(
            &conn,
            &NewSealed {
                product_id: 5001,
                purchase_price: Some(49.99),
                ..Default::default()
            },
        )
        .unwrap();

        let entry = get(&conn, id).unwrap().unwrap();
        assert_eq!(entry.name, "151 Elite Trainer Box");
        assert_eq!(entry.quantity, 1);
        assert_eq!(entry.status, "owned");

        // Dispose by setting status + a sale price.
        assert!(
            update(
                &conn,
                id,
                &SealedEdit {
                    status: Some("opened".into()),
                    ..Default::default()
                }
            )
            .unwrap()
        );
        assert_eq!(get(&conn, id).unwrap().unwrap().status, "opened");

        assert_eq!(list(&conn).unwrap().len(), 1);
        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
    }

    /// The list/get queries surface a market price via `latest_sealed_prices`
    /// joined on `tcgplayer_product_id = product_id`, preferring the market
    /// price and falling back to the mid price. Products with no snapshot
    /// report `None` (no silent zero).
    #[test]
    fn market_price_join_prefers_market_then_mid() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            // 5001 has a full snapshot; 5002 has only a mid price (market
            // NULL, as TCGCSV often reports for sealed); 5003 has no snapshot.
            c.execute(
                "INSERT INTO sealed_products (product_id, name, category, fetched_at) VALUES \
                   (5001, '151 Elite Trainer Box', 'elite_trainer_box', '2026-05-18'), \
                   (5002, 'Obsidian Flames Booster Box', 'booster_box', '2026-05-18'), \
                   (5003, 'Paldea Evolved Bundle', 'bundle', '2026-05-18')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO sealed_prices \
                   (tcgplayer_product_id, low_price, mid_price, high_price, market_price, observed_at) \
                 VALUES \
                   (5001, 40.0, 48.0, 60.0, 52.5, '2026-05-01'), \
                   (5001, 41.0, 49.0, 61.0, 99.9, '2026-05-18'), \
                   (5002, 100.0, 120.0, 140.0, NULL, '2026-05-18')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();

        let full = add(
            &conn,
            &NewSealed {
                product_id: 5001,
                ..Default::default()
            },
        )
        .unwrap();
        let mid_only = add(
            &conn,
            &NewSealed {
                product_id: 5002,
                ..Default::default()
            },
        )
        .unwrap();
        let no_price = add(
            &conn,
            &NewSealed {
                product_id: 5003,
                ..Default::default()
            },
        )
        .unwrap();

        // Latest snapshot's market price wins (99.9 from 2026-05-18, not 52.5).
        assert_eq!(get(&conn, full).unwrap().unwrap().market_price, Some(99.9));
        // Market NULL → falls back to the day's mid price.
        assert_eq!(
            get(&conn, mid_only).unwrap().unwrap().market_price,
            Some(120.0)
        );
        // No snapshot → None (not 0).
        assert_eq!(get(&conn, no_price).unwrap().unwrap().market_price, None);

        // The list query surfaces the same values.
        let by_id: std::collections::HashMap<i64, Option<f64>> = list(&conn)
            .unwrap()
            .into_iter()
            .map(|e| (e.id, e.market_price))
            .collect();
        assert_eq!(by_id[&full], Some(99.9));
        assert_eq!(by_id[&mid_only], Some(120.0));
        assert_eq!(by_id[&no_price], None);
    }

    #[test]
    fn add_rejects_unknown_product() {
        let (_d, conn) = conn();
        let err = add(
            &conn,
            &NewSealed {
                product_id: 999999,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }
}
