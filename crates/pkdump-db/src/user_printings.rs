//! Repository for the user-curated "Missing Variant" escape hatch
//! (decision pokedumpster-x7k).
//!
//! Lives in the user DB so it survives catalog rebuilds. Each row
//! represents a printing the user has logged but the catalog doesn't
//! model yet — typically a misprint or undocumented promo. The
//! `printing_id` follows the format `{card_id}-user-{N}`, where N is a
//! per-card sequence that lets a single card carry multiple distinct
//! ad-hoc variants over time.
//!
//! Downstream callers:
//! - `cards::get_card_detail` / `cards::get_card_prices` UNION ALL
//!   user_printings with shared.printings so the card-detail surfaces
//!   see them.
//! - `binder::get_binder_page` UNION ALLs them too, so a card owned only
//!   through a custom variant still highlights as owned in the binder and
//!   the variant shows as its own slot pip (was catalog-only until a user
//!   hit the gap on card/base2/9).
//! - `manual_prices::insert` accepts a user_printing as a valid parent
//!   so price entry plugs into the same gap-fill rule.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{DbError, Result};

/// One user-created ad-hoc printing.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct UserPrinting {
    pub printing_id: String,
    pub card_id: String,
    pub variant: String,
    pub description: Option<String>,
    pub created_at: String,
}

/// Input fields for creating a user_printing.
#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewUserPrinting {
    pub card_id: String,
    /// Free-text description of the off-catalog variant. Optional.
    pub description: Option<String>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<UserPrinting> {
    Ok(UserPrinting {
        printing_id: r.get(0)?,
        card_id: r.get(1)?,
        variant: r.get(2)?,
        description: r.get(3)?,
        created_at: r.get(4)?,
    })
}

const COLS: &str = "printing_id, card_id, variant, description, created_at";

/// Whether a printing_id resolves to a user_printing (used by
/// `manual_prices::insert` as part of the cross-source FK check).
pub fn exists(conn: &Connection, printing_id: &str) -> Result<bool> {
    let n: Option<i64> = conn
        .prepare("SELECT 1 FROM user_printings WHERE printing_id = ?1")?
        .query_row(params![printing_id], |r| r.get(0))
        .optional()?;
    Ok(n.is_some())
}

/// Create a user_printing for `card_id`. Validates the card exists in
/// the attached shared catalog. Assigns a stable id of the form
/// `{card_id}-user-{N}` where N is one past the highest existing N for
/// the same card.
pub fn insert(conn: &Connection, new: &NewUserPrinting) -> Result<UserPrinting> {
    let exists: Option<i64> = conn
        .prepare("SELECT 1 FROM cards WHERE card_id = ?1")?
        .query_row(params![new.card_id], |r| r.get(0))
        .optional()?;
    if exists.is_none() {
        return Err(DbError::NotFound(format!("card {}", new.card_id)));
    }

    // Per-card sequence. We scan existing rows and pick the max suffix +1
    // — the table is tiny (a few rows per card at most) so a sequence
    // table or trigger would be overkill.
    let prefix = format!("{}-user-", new.card_id);
    let max_n: i64 = conn
        .prepare(
            "SELECT COALESCE(MAX(CAST(SUBSTR(printing_id, ?2) AS INTEGER)), 0) \
             FROM user_printings \
             WHERE printing_id LIKE ?1 || '%'",
        )?
        .query_row(params![prefix, (prefix.len() + 1) as i64], |r| r.get(0))?;
    let printing_id = format!("{prefix}{}", max_n + 1);

    conn.execute(
        "INSERT INTO user_printings (printing_id, card_id, description) \
         VALUES (?1, ?2, ?3)",
        params![printing_id, new.card_id, new.description],
    )?;

    let row = conn
        .prepare(&format!(
            "SELECT {COLS} FROM user_printings WHERE printing_id = ?1"
        ))?
        .query_row([&printing_id], from_row)?;
    Ok(row)
}

/// All user_printings for a card, oldest first (matches insertion order).
pub fn list_for_card(conn: &Connection, card_id: &str) -> Result<Vec<UserPrinting>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLS} FROM user_printings WHERE card_id = ?1 ORDER BY created_at, printing_id"
    ))?;
    let rows = stmt.query_map([card_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Delete a user_printing. Refuses if any collection row or
/// manual_prices row still references it — the caller must remove those
/// first. Returns whether a row was deleted.
pub fn delete(conn: &Connection, printing_id: &str) -> Result<bool> {
    let collection_refs: i64 = conn.query_row(
        "SELECT count(*) FROM collection WHERE printing_id = ?1",
        params![printing_id],
        |r| r.get(0),
    )?;
    if collection_refs > 0 {
        return Err(DbError::Conflict(format!(
            "user_printing {printing_id} still has {collection_refs} collection row(s); \
             remove those copies first"
        )));
    }
    // manual_prices is allowed to dangle — the only consequence is dead
    // rows. Cascade-delete them so we don't leave noise behind.
    conn.execute(
        "DELETE FROM manual_prices WHERE printing_id = ?1",
        params![printing_id],
    )?;
    let n = conn.execute(
        "DELETE FROM user_printings WHERE printing_id = ?1",
        params![printing_id],
    )?;
    Ok(n > 0)
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
                "INSERT INTO sets (set_code, name, series) VALUES ('jungle', 'Jungle', 'Base')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('jungle-4', 'jungle', '4', 4, 'Jolteon')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn insert_assigns_per_card_sequence() {
        let (_d, conn) = conn();
        let a = insert(
            &conn,
            &NewUserPrinting {
                card_id: "jungle-4".into(),
                description: Some("no set stamp misprint".into()),
            },
        )
        .unwrap();
        let b = insert(
            &conn,
            &NewUserPrinting {
                card_id: "jungle-4".into(),
                description: Some("another oddity".into()),
            },
        )
        .unwrap();
        assert_eq!(a.printing_id, "jungle-4-user-1");
        assert_eq!(b.printing_id, "jungle-4-user-2");
        assert_eq!(a.variant, "missing_variant");
        assert_eq!(a.description.as_deref(), Some("no set stamp misprint"));
    }

    #[test]
    fn insert_rejects_unknown_card() {
        let (_d, conn) = conn();
        let err = insert(
            &conn,
            &NewUserPrinting {
                card_id: "nope-1".into(),
                description: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }

    #[test]
    fn list_for_card_returns_inserted_rows() {
        let (_d, conn) = conn();
        insert(
            &conn,
            &NewUserPrinting {
                card_id: "jungle-4".into(),
                description: Some("misprint".into()),
            },
        )
        .unwrap();
        let rows = list_for_card(&conn, "jungle-4").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].description.as_deref(), Some("misprint"));
    }

    #[test]
    fn exists_covers_user_printings() {
        let (_d, conn) = conn();
        let row = insert(
            &conn,
            &NewUserPrinting {
                card_id: "jungle-4".into(),
                description: None,
            },
        )
        .unwrap();
        assert!(exists(&conn, &row.printing_id).unwrap());
        assert!(!exists(&conn, "nope").unwrap());
    }

    #[test]
    fn delete_refuses_when_collection_references_it() {
        let (_d, mut conn) = conn();
        let row = insert(
            &conn,
            &NewUserPrinting {
                card_id: "jungle-4".into(),
                description: None,
            },
        )
        .unwrap();
        crate::collection::add(
            &mut conn,
            &crate::collection::NewCopy {
                printing_id: row.printing_id.clone(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let err = delete(&conn, &row.printing_id).unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)));
    }

    #[test]
    fn delete_cascades_manual_prices() {
        let (_d, conn) = conn();
        let row = insert(
            &conn,
            &NewUserPrinting {
                card_id: "jungle-4".into(),
                description: None,
            },
        )
        .unwrap();
        crate::manual_prices::insert(
            &conn,
            &crate::manual_prices::NewManualPrice {
                printing_id: row.printing_id.clone(),
                price: 200.0,
                observed_at: None,
                note: None,
            },
        )
        .unwrap();
        assert!(delete(&conn, &row.printing_id).unwrap());
        let still_there = crate::manual_prices::list_for_printing(&conn, &row.printing_id).unwrap();
        assert!(still_there.is_empty());
    }
}
