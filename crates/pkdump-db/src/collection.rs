//! Repository for the `collection` table — one row per physical card owned.
//!
//! Catalog references (`printing_id`) are validated against the attached
//! shared catalog before a write (PLAN.md §3.5). Status changes append to
//! `status_log` and binder/deck moves append to `movement_log`, so the
//! collection carries a full audit trail.

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog;
use crate::error::{DbError, Result};

const COLUMNS: &str = "id, printing_id, condition, language, purchase_price, \
     sale_price, acquired_at, source, notes, tags, graded, grade_company, \
     grade_value, grade_cert, status, order_id, binder_id, deck_id, batch_id";

/// A row from the `collection` table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct CollectionEntry {
    #[ts(type = "number")]
    pub id: i64,
    pub printing_id: String,
    pub condition: String,
    pub language: String,
    pub purchase_price: Option<f64>,
    pub sale_price: Option<f64>,
    pub acquired_at: String,
    pub source: String,
    pub notes: Option<String>,
    pub tags: Option<String>,
    pub graded: bool,
    pub grade_company: Option<String>,
    pub grade_value: Option<f64>,
    pub grade_cert: Option<String>,
    pub status: String,
    #[ts(type = "number | null")]
    pub order_id: Option<i64>,
    #[ts(type = "number | null")]
    pub binder_id: Option<i64>,
    #[ts(type = "number | null")]
    pub deck_id: Option<i64>,
    #[ts(type = "number | null")]
    pub batch_id: Option<i64>,
}

/// Fields supplied when adding a copy to the collection. Optional fields
/// fall back to schema defaults.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewCopy {
    pub printing_id: String,
    pub condition: Option<String>,
    pub language: Option<String>,
    pub purchase_price: Option<f64>,
    pub acquired_at: Option<String>,
    pub source: String,
    pub status: Option<String>,
    pub notes: Option<String>,
    #[ts(type = "number | null")]
    pub order_id: Option<i64>,
    #[ts(type = "number | null")]
    pub binder_id: Option<i64>,
    #[ts(type = "number | null")]
    pub deck_id: Option<i64>,
    #[ts(type = "number | null")]
    pub batch_id: Option<i64>,
}

/// Editable per-copy fields. A `None` field is left unchanged.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct CopyEdit {
    pub condition: Option<String>,
    pub language: Option<String>,
    pub purchase_price: Option<f64>,
    pub sale_price: Option<f64>,
    pub notes: Option<String>,
    pub tags: Option<String>,
    pub graded: Option<bool>,
    pub grade_company: Option<String>,
    pub grade_value: Option<f64>,
    pub grade_cert: Option<String>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<CollectionEntry> {
    Ok(CollectionEntry {
        id: r.get(0)?,
        printing_id: r.get(1)?,
        condition: r.get(2)?,
        language: r.get(3)?,
        purchase_price: r.get(4)?,
        sale_price: r.get(5)?,
        acquired_at: r.get(6)?,
        source: r.get(7)?,
        notes: r.get(8)?,
        tags: r.get(9)?,
        graded: r.get(10)?,
        grade_company: r.get(11)?,
        grade_value: r.get(12)?,
        grade_cert: r.get(13)?,
        status: r.get(14)?,
        order_id: r.get(15)?,
        binder_id: r.get(16)?,
        deck_id: r.get(17)?,
        batch_id: r.get(18)?,
    })
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Add a copy to the collection. Validates the printing against the catalog,
/// rejects a binder+deck conflict, and records the opening `status_log`
/// entry. Returns the new row id.
pub fn add(conn: &mut Connection, new: &NewCopy) -> Result<i64> {
    if !catalog::printing_exists(conn, &new.printing_id)? {
        return Err(DbError::NotFound(format!(
            "printing '{}' is not in the catalog",
            new.printing_id
        )));
    }
    if new.binder_id.is_some() && new.deck_id.is_some() {
        return Err(DbError::Conflict(
            "a copy cannot be added to a binder and a deck at once".into(),
        ));
    }
    let condition = new.condition.as_deref().unwrap_or("Near Mint");
    let language = new.language.as_deref().unwrap_or("English");
    let status = new.status.as_deref().unwrap_or("owned");
    let acquired = new.acquired_at.clone().unwrap_or_else(now);

    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO collection
           (printing_id, condition, language, purchase_price, acquired_at,
            source, status, notes, order_id, binder_id, deck_id, batch_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            new.printing_id,
            condition,
            language,
            new.purchase_price,
            acquired,
            new.source,
            status,
            new.notes,
            new.order_id,
            new.binder_id,
            new.deck_id,
            new.batch_id,
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO status_log (collection_id, from_status, to_status, changed_at, note)
         VALUES (?1, NULL, ?2, ?3, 'added')",
        params![id, status, now()],
    )?;
    tx.commit()?;
    Ok(id)
}

/// Fetch a single collection entry by id.
pub fn get(conn: &Connection, id: i64) -> Result<Option<CollectionEntry>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM collection WHERE id = ?1"))?;
    Ok(stmt.query_row([id], from_row).optional()?)
}

/// List collection entries, newest first.
pub fn list(conn: &Connection, limit: i64, offset: i64) -> Result<Vec<CollectionEntry>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM collection ORDER BY id DESC LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt.query_map([limit, offset], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A collection entry joined to its printing and card — the display-ready
/// shape the `/collection` UI renders.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CollectionRow {
    #[ts(type = "number")]
    pub id: i64,
    pub printing_id: String,
    pub condition: String,
    pub language: String,
    pub purchase_price: Option<f64>,
    pub sale_price: Option<f64>,
    pub acquired_at: String,
    pub source: String,
    pub notes: Option<String>,
    pub status: String,
    pub graded: bool,
    #[ts(type = "number | null")]
    pub binder_id: Option<i64>,
    #[ts(type = "number | null")]
    pub deck_id: Option<i64>,
    pub variant: String,
    pub card_id: String,
    pub set_code: String,
    /// The collector-facing 3-letter set code (e.g. "MEW", "PFL"), if known.
    pub set_ptcgo_code: Option<String>,
    pub set_symbol_url: Option<String>,
    pub number: String,
    pub name: String,
    pub rarity: Option<String>,
    /// `Pokémon` | `Trainer` | `Energy`.
    pub supertype: Option<String>,
    /// JSON array, e.g. `["Supporter"]` or `["Basic","ex"]`.
    pub subtypes: Option<String>,
    /// JSON array of the card's energy types, e.g. `["Fire"]`.
    pub types: Option<String>,
    /// JSON array of attacks, each with a `cost` energy list — the table
    /// renders one pip line per attack.
    pub attacks: Option<String>,
    pub image_small: Option<String>,
}

const ROW_COLUMNS: &str = "c.id, c.printing_id, c.condition, c.language, \
     c.purchase_price, c.sale_price, c.acquired_at, c.source, c.notes, \
     c.status, c.graded, c.binder_id, c.deck_id, p.variant, cd.card_id, \
     cd.set_code, s.ptcgo_code, s.symbol_url, cd.number, cd.name, \
     cd.rarity, cd.supertype, cd.subtypes, cd.types, cd.attacks, \
     cd.image_small";

const ROW_FROM: &str = "FROM collection c \
     JOIN printings p ON c.printing_id = p.printing_id \
     JOIN cards cd ON p.card_id = cd.card_id \
     JOIN sets s ON cd.set_code = s.set_code";

fn collection_row_from_row(r: &rusqlite::Row) -> rusqlite::Result<CollectionRow> {
    Ok(CollectionRow {
        id: r.get(0)?,
        printing_id: r.get(1)?,
        condition: r.get(2)?,
        language: r.get(3)?,
        purchase_price: r.get(4)?,
        sale_price: r.get(5)?,
        acquired_at: r.get(6)?,
        source: r.get(7)?,
        notes: r.get(8)?,
        status: r.get(9)?,
        graded: r.get(10)?,
        binder_id: r.get(11)?,
        deck_id: r.get(12)?,
        variant: r.get(13)?,
        card_id: r.get(14)?,
        set_code: r.get(15)?,
        set_ptcgo_code: r.get(16)?,
        set_symbol_url: r.get(17)?,
        number: r.get(18)?,
        name: r.get(19)?,
        rarity: r.get(20)?,
        supertype: r.get(21)?,
        subtypes: r.get(22)?,
        types: r.get(23)?,
        attacks: r.get(24)?,
        image_small: r.get(25)?,
    })
}

/// List collection entries as display rows (joined to printing + card),
/// newest first. Requires the catalog to be attached (a user connection).
pub fn list_rows(conn: &Connection, limit: i64, offset: i64) -> Result<Vec<CollectionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} {ROW_FROM} ORDER BY c.id DESC LIMIT ?1 OFFSET ?2"
    ))?;
    let rows = stmt.query_map([limit, offset], collection_row_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Fetch a single collection entry as a display row.
pub fn get_row(conn: &Connection, id: i64) -> Result<Option<CollectionRow>> {
    let mut stmt = conn.prepare(&format!("SELECT {ROW_COLUMNS} {ROW_FROM} WHERE c.id = ?1"))?;
    Ok(stmt.query_row([id], collection_row_from_row).optional()?)
}

/// List the display rows assigned to a binder.
pub fn list_by_binder(conn: &Connection, binder_id: i64) -> Result<Vec<CollectionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} {ROW_FROM} WHERE c.binder_id = ?1 ORDER BY c.id"
    ))?;
    let rows = stmt.query_map([binder_id], collection_row_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// List the display rows assigned to a deck.
pub fn list_by_deck(conn: &Connection, deck_id: i64) -> Result<Vec<CollectionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} {ROW_FROM} WHERE c.deck_id = ?1 ORDER BY c.id"
    ))?;
    let rows = stmt.query_map([deck_id], collection_row_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// List the display rows belonging to an order.
pub fn list_by_order(conn: &Connection, order_id: i64) -> Result<Vec<CollectionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} {ROW_FROM} WHERE c.order_id = ?1 ORDER BY c.id"
    ))?;
    let rows = stmt.query_map([order_id], collection_row_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// List the display rows belonging to an ingest batch.
pub fn list_by_batch(conn: &Connection, batch_id: i64) -> Result<Vec<CollectionRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ROW_COLUMNS} {ROW_FROM} WHERE c.batch_id = ?1 ORDER BY c.id"
    ))?;
    let rows = stmt.query_map([batch_id], collection_row_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// List every copy the user owns of any printing of a given card.
pub fn list_for_card(conn: &Connection, card_id: &str) -> Result<Vec<CollectionEntry>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM collection \
         WHERE printing_id IN (SELECT printing_id FROM printings WHERE card_id = ?1) \
         ORDER BY id"
    ))?;
    let rows = stmt.query_map([card_id], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Update editable per-copy fields. Returns whether a row was changed.
/// (A `None` field is kept; this path cannot clear a field to NULL.)
pub fn update(conn: &Connection, id: i64, edit: &CopyEdit) -> Result<bool> {
    let n = conn.execute(
        "UPDATE collection SET
           condition      = COALESCE(?2, condition),
           language       = COALESCE(?3, language),
           purchase_price = COALESCE(?4, purchase_price),
           sale_price     = COALESCE(?5, sale_price),
           notes          = COALESCE(?6, notes),
           tags           = COALESCE(?7, tags),
           graded         = COALESCE(?8, graded),
           grade_company  = COALESCE(?9, grade_company),
           grade_value    = COALESCE(?10, grade_value),
           grade_cert     = COALESCE(?11, grade_cert)
         WHERE id = ?1",
        params![
            id,
            edit.condition,
            edit.language,
            edit.purchase_price,
            edit.sale_price,
            edit.notes,
            edit.tags,
            edit.graded,
            edit.grade_company,
            edit.grade_value,
            edit.grade_cert,
        ],
    )?;
    Ok(n > 0)
}

/// Change a copy's lifecycle status, appending a `status_log` entry.
pub fn set_status(
    conn: &mut Connection,
    id: i64,
    new_status: &str,
    note: Option<&str>,
) -> Result<()> {
    let from: String = conn
        .query_row("SELECT status FROM collection WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))?;

    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE collection SET status = ?2 WHERE id = ?1",
        params![id, new_status],
    )?;
    tx.execute(
        "INSERT INTO status_log (collection_id, from_status, to_status, changed_at, note)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, from, new_status, now(), note],
    )?;
    tx.commit()?;
    Ok(())
}

/// Move a copy to a binder, a deck, or neither, appending a `movement_log`
/// entry. Rejects an attempt to set both a binder and a deck.
pub fn move_to(
    conn: &mut Connection,
    id: i64,
    binder_id: Option<i64>,
    deck_id: Option<i64>,
    note: Option<&str>,
) -> Result<()> {
    if binder_id.is_some() && deck_id.is_some() {
        return Err(DbError::Conflict(
            "a copy cannot be in a binder and a deck at once".into(),
        ));
    }
    let (from_binder, from_deck): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT binder_id, deck_id FROM collection WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))?;

    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE collection SET binder_id = ?2, deck_id = ?3 WHERE id = ?1",
        params![id, binder_id, deck_id],
    )?;
    tx.execute(
        "INSERT INTO movement_log
           (collection_id, from_binder_id, to_binder_id, from_deck_id,
            to_deck_id, changed_at, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, from_binder, binder_id, from_deck, deck_id, now(), note],
    )?;
    tx.commit()?;
    Ok(())
}

/// Delete a collection entry. Its `status_log` and `movement_log` rows
/// cascade away. Returns whether a row was removed.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM collection WHERE id = ?1", [id])? > 0)
}

/// Delete the most recently added copy of a printing — the binder modal's
/// "−" action. Returns false when no copy of that printing exists.
pub fn delete_latest_for_printing(conn: &Connection, printing_id: &str) -> Result<bool> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM collection WHERE printing_id = ?1 ORDER BY id DESC LIMIT 1",
            [printing_id],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => delete(conn, id),
        None => Ok(false),
    }
}

/// Change which printing a copy is — for correcting a mis-logged variant.
/// The new printing must belong to the *same card* (you can swap a copy
/// between a card's variants, not to a different card).
pub fn change_printing(conn: &Connection, id: i64, new_printing_id: &str) -> Result<()> {
    let current: String = conn
        .query_row(
            "SELECT printing_id FROM collection WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("collection entry {id}")))?;

    let card_of = |printing_id: &str| -> Result<Option<String>> {
        Ok(conn
            .query_row(
                "SELECT card_id FROM printings WHERE printing_id = ?1",
                [printing_id],
                |r| r.get(0),
            )
            .optional()?)
    };
    let new_card = card_of(new_printing_id)?.ok_or_else(|| {
        DbError::NotFound(format!(
            "printing '{new_printing_id}' is not in the catalog"
        ))
    })?;
    if card_of(&current)?.as_deref() != Some(new_card.as_str()) {
        return Err(DbError::Conflict(
            "a copy can only be changed to another printing of the same card".into(),
        ));
    }

    conn.execute(
        "UPDATE collection SET printing_id = ?2 WHERE id = ?1",
        params![id, new_printing_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};

    /// A user connection whose catalog holds one printing, `sv3pt5-1-normal`.
    fn user_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-1-normal', 'sv3pt5-1', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn a_copy() -> NewCopy {
        NewCopy {
            printing_id: "sv3pt5-1-normal".into(),
            source: "manual_id".into(),
            ..Default::default()
        }
    }

    #[test]
    fn add_and_get_round_trip() {
        let (_d, mut conn) = user_conn();
        let id = add(&mut conn, &a_copy()).unwrap();

        let entry = get(&conn, id).unwrap().unwrap();
        assert_eq!(entry.printing_id, "sv3pt5-1-normal");
        assert_eq!(entry.condition, "Near Mint"); // default
        assert_eq!(entry.status, "owned"); // default

        // The opening status_log entry was recorded.
        let logs: i64 = conn
            .query_row(
                "SELECT count(*) FROM status_log WHERE collection_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logs, 1);
    }

    #[test]
    fn add_rejects_unknown_printing() {
        let (_d, mut conn) = user_conn();
        let err = add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-mysteryfoil".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }

    #[test]
    fn update_changes_editable_fields() {
        let (_d, mut conn) = user_conn();
        let id = add(&mut conn, &a_copy()).unwrap();
        update(
            &conn,
            id,
            &CopyEdit {
                condition: Some("Lightly Played".into()),
                purchase_price: Some(3.50),
                ..Default::default()
            },
        )
        .unwrap();
        let entry = get(&conn, id).unwrap().unwrap();
        assert_eq!(entry.condition, "Lightly Played");
        assert_eq!(entry.purchase_price, Some(3.50));
        assert_eq!(entry.language, "English"); // untouched
    }

    #[test]
    fn set_status_appends_to_log() {
        let (_d, mut conn) = user_conn();
        let id = add(&mut conn, &a_copy()).unwrap();
        set_status(&mut conn, id, "sold", Some("eBay")).unwrap();

        assert_eq!(get(&conn, id).unwrap().unwrap().status, "sold");
        let logs: i64 = conn
            .query_row(
                "SELECT count(*) FROM status_log WHERE collection_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logs, 2); // 'added' + the sale
    }

    #[test]
    fn move_to_logs_and_rejects_both() {
        let (_d, mut conn) = user_conn();
        conn.execute(
            "INSERT INTO binders (id, name, created_at, updated_at) \
             VALUES (1, 'Trade Binder', '2026-05-18', '2026-05-18')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO decks (id, name, created_at, updated_at) \
             VALUES (1, 'Alice deck', '2026-05-18', '2026-05-18')",
            [],
        )
        .unwrap();
        let id = add(&mut conn, &a_copy()).unwrap();

        move_to(&mut conn, id, Some(1), None, None).unwrap();
        assert_eq!(get(&conn, id).unwrap().unwrap().binder_id, Some(1));
        let moves: i64 = conn
            .query_row(
                "SELECT count(*) FROM movement_log WHERE collection_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(moves, 1);

        let err = move_to(&mut conn, id, Some(1), Some(1), None).unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)));
    }

    #[test]
    fn delete_cascades_audit_logs() {
        let (_d, mut conn) = user_conn();
        let id = add(&mut conn, &a_copy()).unwrap();
        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());

        let logs: i64 = conn
            .query_row(
                "SELECT count(*) FROM status_log WHERE collection_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(logs, 0, "status_log rows cascade on delete");
    }

    #[test]
    fn list_paginates_newest_first() {
        let (_d, mut conn) = user_conn();
        let first = add(&mut conn, &a_copy()).unwrap();
        let second = add(&mut conn, &a_copy()).unwrap();

        let page = list(&conn, 1, 0).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, second); // newest first

        let page2 = list(&conn, 1, 1).unwrap();
        assert_eq!(page2[0].id, first);
    }

    #[test]
    fn change_printing_swaps_within_card_and_rejects_cross_card() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'SV')",
                [],
            )
            .unwrap();
            for n in ["1", "2"] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, 'sv3pt5', ?2, ?3, 'Card')",
                    rusqlite::params![format!("sv3pt5-{n}"), n, n.parse::<i64>().unwrap()],
                )
                .unwrap();
            }
            for (pid, card, variant) in [
                ("sv3pt5-1-normal", "sv3pt5-1", "normal"),
                ("sv3pt5-1-reverse_holo", "sv3pt5-1", "reverse_holo"),
                ("sv3pt5-2-normal", "sv3pt5-2", "normal"),
            ] {
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) VALUES (?1, ?2, ?3)",
                    rusqlite::params![pid, card, variant],
                )
                .unwrap();
            }
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let id = add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        // Swapping to another variant of the same card succeeds.
        change_printing(&conn, id, "sv3pt5-1-reverse_holo").unwrap();
        assert_eq!(
            get(&conn, id).unwrap().unwrap().printing_id,
            "sv3pt5-1-reverse_holo"
        );

        // Swapping to a printing of a different card is rejected.
        let err = change_printing(&conn, id, "sv3pt5-2-normal").unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)));
    }
}
