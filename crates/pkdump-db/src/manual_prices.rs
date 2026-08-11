//! Repository for user-entered manual prices — **for printings the user
//! invented, and only those** (`user_printings`).
//!
//! Lives in the user DB (`<user>.sqlite`). It used to accept any printing,
//! catalog rows included, and that was the wrong shape twice over (pd-m4gw):
//! a catalog printing the feed cannot price is missing for *every* tenant, so
//! each one had to rediscover the same gap by hand; and pricing a catalog row
//! from the tenant forced `ORDER BY price` to `COALESCE` across the `ATTACH`
//! boundary, which no index can satisfy. Catalog gaps are now patched in the
//! catalog — see [`crate::catalog_prices`].
//!
//! What is left here is the one case the catalog genuinely cannot know about:
//! a printing that is not in the catalog *because a user added it by hand*.
//! [`insert`] refuses anything else rather than accepting a row nothing would
//! ever read.
//!
//! **Effective-price rule.** Defined once, in [`crate::prices`]: TCGplayer
//! market wins, then the curated catalog override, then — for a user-created
//! printing only — the newest row here. Time-series visualisation still
//! surfaces every source, so a manual entry stays visible alongside the
//! TCGplayer line.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{DbError, Result};

/// One manual price observation for a printing.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ManualPrice {
    #[ts(type = "number")]
    pub id: i64,
    pub printing_id: String,
    pub price: f64,
    pub observed_at: String,
    pub note: Option<String>,
    pub created_at: String,
}

/// Input fields for creating a manual price.
#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewManualPrice {
    pub printing_id: String,
    pub price: f64,
    /// ISO-8601 timestamp. `None` → defaults to now (UTC).
    pub observed_at: Option<String>,
    pub note: Option<String>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<ManualPrice> {
    Ok(ManualPrice {
        id: r.get(0)?,
        printing_id: r.get(1)?,
        price: r.get(2)?,
        observed_at: r.get(3)?,
        note: r.get(4)?,
        created_at: r.get(5)?,
    })
}

const COLS: &str = "id, printing_id, price, observed_at, note, created_at";

/// Why a catalog printing is refused, quoted back to the operator. The
/// message names the file to edit, because "the catalog is wrong" is only
/// actionable if you know where the catalog is authored.
pub const CATALOG_PRINTING_RULE: &str = "a manual price belongs to a user-created printing; a catalog printing \
     that TCGplayer does not price is a catalog gap, patched for every tenant \
     in data/overrides/catalog_prices.json";

/// Insert a manual price; returns its id.
///
/// The printing must exist in the user DB's `user_printings` — the app-layer
/// FK check, since SQLite cannot enforce cross-database FKs.
///
/// A **catalog** printing is refused with [`DbError::Invalid`] rather than
/// silently accepted: since pd-m4gw nothing reads a tenant manual price for a
/// catalog row, so storing one would be a write the user could watch fail to
/// change the price. A printing that is in neither table is still
/// [`DbError::NotFound`] — the two are different mistakes and get different
/// answers.
pub fn insert(conn: &Connection, new: &NewManualPrice) -> Result<i64> {
    let invented: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM user_printings WHERE printing_id = ?1)",
        params![new.printing_id],
        |r| r.get::<_, i64>(0),
    )? == 1;
    if !invented {
        let in_catalog: Option<i64> = conn
            .prepare("SELECT 1 FROM printings WHERE printing_id = ?1")?
            .query_row(params![new.printing_id], |r| r.get(0))
            .optional()?;
        return Err(match in_catalog {
            Some(_) => DbError::Invalid(format!(
                "{} is a catalog printing — {CATALOG_PRINTING_RULE}",
                new.printing_id
            )),
            None => DbError::NotFound(format!("printing {}", new.printing_id)),
        });
    }

    let observed_at = new
        .observed_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());

    conn.execute(
        "INSERT INTO manual_prices (printing_id, price, observed_at, note) \
         VALUES (?1, ?2, ?3, ?4)",
        params![new.printing_id, new.price, observed_at, new.note],
    )?;
    Ok(conn.last_insert_rowid())
}

/// All manual prices for a printing, newest first.
pub fn list_for_printing(conn: &Connection, printing_id: &str) -> Result<Vec<ManualPrice>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM manual_prices \
         WHERE printing_id = ?1 \
         ORDER BY observed_at DESC, id DESC"
    ))?;
    let rows = stmt.query_map([printing_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Delete a single manual-price entry. Returns whether a row was removed.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    let n = conn.execute("DELETE FROM manual_prices WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};

    /// A catalog holding `basep-14-normal`, plus a tenant that has invented
    /// `basep-14-user-1` of its own. `PRINTING` is the one these tests may
    /// price; the catalog printing is here to prove it is refused.
    const PRINTING: &str = "basep-14-user-1";

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('basep', 'Promos', 'Base')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('basep-14', 'basep', '14', 14, 'Mewtwo')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('basep-14-normal', 'basep-14', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        conn.execute(
            "INSERT INTO user_printings (printing_id, card_id, variant, created_at) \
             VALUES (?1, 'basep-14', 'missing_variant', '2026-08-10')",
            [PRINTING],
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn insert_defaults_observed_at_to_now() {
        let (_d, conn) = conn();
        let id = insert(
            &conn,
            &NewManualPrice {
                printing_id: PRINTING.into(),
                price: 200.0,
                observed_at: None,
                note: Some("test".into()),
            },
        )
        .unwrap();
        assert!(id > 0);
        let rows = list_for_printing(&conn, PRINTING).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].price, 200.0);
        // Default observed_at should be a recent timestamp — within
        // the last minute.
        let now = chrono::Utc::now();
        let parsed = chrono::DateTime::parse_from_rfc3339(&rows[0].observed_at).unwrap();
        let delta = (now - parsed.with_timezone(&chrono::Utc))
            .num_seconds()
            .abs();
        assert!(
            delta < 60,
            "observed_at not recent: {}",
            rows[0].observed_at
        );
    }

    #[test]
    fn insert_honors_explicit_observed_at() {
        let (_d, conn) = conn();
        insert(
            &conn,
            &NewManualPrice {
                printing_id: PRINTING.into(),
                price: 175.0,
                observed_at: Some("2024-03-01T00:00:00Z".into()),
                note: None,
            },
        )
        .unwrap();
        let rows = list_for_printing(&conn, PRINTING).unwrap();
        assert_eq!(rows[0].observed_at, "2024-03-01T00:00:00Z");
    }

    #[test]
    fn list_returns_newest_first() {
        let (_d, conn) = conn();
        for (t, p) in [
            ("2024-01-01T00:00:00Z", 100.0),
            ("2024-06-01T00:00:00Z", 150.0),
            ("2024-03-01T00:00:00Z", 125.0),
        ] {
            insert(
                &conn,
                &NewManualPrice {
                    printing_id: PRINTING.into(),
                    price: p,
                    observed_at: Some(t.into()),
                    note: None,
                },
            )
            .unwrap();
        }
        let rows = list_for_printing(&conn, PRINTING).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].observed_at, "2024-06-01T00:00:00Z");
        assert_eq!(rows[2].observed_at, "2024-01-01T00:00:00Z");
    }

    /// pd-m4gw: a catalog printing's price is the catalog's business. Before
    /// this change the insert succeeded and the row then priced the printing
    /// for this tenant alone.
    #[test]
    fn insert_refuses_a_catalog_printing_and_says_where_the_price_belongs() {
        let (_d, conn) = conn();
        let err = insert(
            &conn,
            &NewManualPrice {
                printing_id: "basep-14-normal".into(),
                price: 29.0,
                observed_at: None,
                note: None,
            },
        )
        .unwrap_err();
        // Not NotFound — the printing exists, it just isn't ours to price.
        assert!(matches!(err, DbError::Invalid(_)), "{err:?}");
        assert!(
            err.to_string()
                .contains("data/overrides/catalog_prices.json"),
            "the refusal must name the file to edit: {err}"
        );
        assert!(
            list_for_printing(&conn, "basep-14-normal")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn insert_rejects_unknown_printing() {
        let (_d, conn) = conn();
        let err = insert(
            &conn,
            &NewManualPrice {
                printing_id: "nope-0-normal".into(),
                price: 1.0,
                observed_at: None,
                note: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }

    #[test]
    fn delete_removes_row() {
        let (_d, conn) = conn();
        let id = insert(
            &conn,
            &NewManualPrice {
                printing_id: PRINTING.into(),
                price: 50.0,
                observed_at: None,
                note: None,
            },
        )
        .unwrap();
        assert!(delete(&conn, id).unwrap());
        assert!(!delete(&conn, id).unwrap());
        assert!(list_for_printing(&conn, PRINTING).unwrap().is_empty());
    }
}
