//! Repository for the wishlist — cards the user wants to acquire.

use rusqlite::{Connection, params};

use crate::catalog;
use crate::error::{DbError, Result};

const COLS: &str = "w.id, w.card_id, w.printing_id, w.max_price, w.priority, \
     w.notes, w.added_at, w.source, w.fulfilled_at, \
     cd.set_code, cd.number, cd.name, cd.rarity, cd.image_small";

/// A wishlist entry joined to its card.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct WishlistEntry {
    #[ts(type = "number")]
    pub id: i64,
    pub card_id: String,
    /// A specific wanted printing, or null for "any printing".
    pub printing_id: Option<String>,
    pub max_price: Option<f64>,
    #[ts(type = "number")]
    pub priority: i64,
    pub notes: Option<String>,
    pub added_at: String,
    pub source: String,
    pub fulfilled_at: Option<String>,
    pub set_code: String,
    pub number: String,
    pub name: String,
    pub rarity: Option<String>,
    pub image_small: Option<String>,
}

/// Fields for adding a wish.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewWish {
    pub card_id: String,
    pub printing_id: Option<String>,
    pub max_price: Option<f64>,
    #[ts(type = "number | null")]
    pub priority: Option<i64>,
    pub notes: Option<String>,
}

/// Editable wish fields. A `None` field is left unchanged.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct WishEdit {
    pub max_price: Option<f64>,
    #[ts(type = "number | null")]
    pub priority: Option<i64>,
    pub notes: Option<String>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<WishlistEntry> {
    Ok(WishlistEntry {
        id: r.get(0)?,
        card_id: r.get(1)?,
        printing_id: r.get(2)?,
        max_price: r.get(3)?,
        priority: r.get(4)?,
        notes: r.get(5)?,
        added_at: r.get(6)?,
        source: r.get(7)?,
        fulfilled_at: r.get(8)?,
        set_code: r.get(9)?,
        number: r.get(10)?,
        name: r.get(11)?,
        rarity: r.get(12)?,
        image_small: r.get(13)?,
    })
}

/// Add a wish. Validates the card against the catalog. Returns the new id.
pub fn add(conn: &Connection, new: &NewWish) -> Result<i64> {
    if !catalog::card_exists(conn, &new.card_id)? {
        return Err(DbError::NotFound(format!(
            "card '{}' is not in the catalog",
            new.card_id
        )));
    }
    conn.execute(
        "INSERT INTO wishlist \
           (card_id, printing_id, max_price, priority, notes, added_at, source) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'manual')",
        params![
            new.card_id,
            new.printing_id,
            new.max_price,
            new.priority.unwrap_or(0),
            new.notes,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List wishlist entries. `include_fulfilled` false returns only active wishes.
/// Ordered by priority (highest first), then most-recently added.
pub fn list(conn: &Connection, include_fulfilled: bool) -> Result<Vec<WishlistEntry>> {
    let filter = if include_fulfilled {
        ""
    } else {
        "WHERE w.fulfilled_at IS NULL "
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM wishlist w JOIN cards cd ON w.card_id = cd.card_id \
         {filter}ORDER BY w.priority DESC, w.id DESC"
    ))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Update a wish's editable fields. Returns whether a row changed.
pub fn update(conn: &Connection, id: i64, edit: &WishEdit) -> Result<bool> {
    let n = conn.execute(
        "UPDATE wishlist SET \
           max_price = COALESCE(?2, max_price), \
           priority  = COALESCE(?3, priority), \
           notes     = COALESCE(?4, notes) \
         WHERE id = ?1",
        params![id, edit.max_price, edit.priority, edit.notes],
    )?;
    Ok(n > 0)
}

/// Mark a wish fulfilled (or clear it). Returns whether a row changed.
pub fn set_fulfilled(conn: &Connection, id: i64, fulfilled: bool) -> Result<bool> {
    let stamp = fulfilled.then(|| chrono::Utc::now().to_rfc3339());
    let n = conn.execute(
        "UPDATE wishlist SET fulfilled_at = ?2 WHERE id = ?1",
        params![id, stamp],
    )?;
    Ok(n > 0)
}

/// Delete a wish.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM wishlist WHERE id = ?1", [id])? > 0)
}

/// Whether the card already has an (active) wish — used to avoid duplicates.
pub fn exists_for_card(conn: &Connection, card_id: &str) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM wishlist WHERE card_id = ?1 AND fulfilled_at IS NULL")?
        .exists([card_id])?)
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
                "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'SV')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn add_list_fulfill_delete() {
        let (_d, conn) = conn();
        let id = add(
            &conn,
            &NewWish {
                card_id: "sv3pt5-6".into(),
                priority: Some(5),
                max_price: Some(200.0),
                ..Default::default()
            },
        )
        .unwrap();

        let active = list(&conn, false).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].name, "Charizard ex");
        assert_eq!(active[0].priority, 5);
        assert!(exists_for_card(&conn, "sv3pt5-6").unwrap());

        // Fulfilled wishes drop out of the active list.
        assert!(set_fulfilled(&conn, id, true).unwrap());
        assert_eq!(list(&conn, false).unwrap().len(), 0);
        assert_eq!(list(&conn, true).unwrap().len(), 1);
        assert!(!exists_for_card(&conn, "sv3pt5-6").unwrap());

        assert!(delete(&conn, id).unwrap());
        assert_eq!(list(&conn, true).unwrap().len(), 0);
    }

    #[test]
    fn add_rejects_unknown_card() {
        let (_d, conn) = conn();
        let err = add(
            &conn,
            &NewWish {
                card_id: "nope-1".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }
}
