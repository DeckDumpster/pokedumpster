//! Repository for the `binders` table — physical card binders.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;

const COLS: &str = "b.id, b.name, b.description, b.color, b.binder_type, \
     b.pocket_size, b.storage_location, b.created_at, b.updated_at, \
     (SELECT count(*) FROM collection c WHERE c.binder_id = b.id)";

/// A binder, with the count of cards assigned to it.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Binder {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub binder_type: Option<String>,
    #[ts(type = "number")]
    pub pocket_size: i64,
    pub storage_location: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[ts(type = "number")]
    pub card_count: i64,
}

/// Fields for creating a binder.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewBinder {
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub binder_type: Option<String>,
    #[ts(type = "number | null")]
    pub pocket_size: Option<i64>,
    pub storage_location: Option<String>,
}

/// Editable binder fields. A `None` field is left unchanged.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct BinderEdit {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub binder_type: Option<String>,
    #[ts(type = "number | null")]
    pub pocket_size: Option<i64>,
    pub storage_location: Option<String>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Binder> {
    Ok(Binder {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        color: r.get(3)?,
        binder_type: r.get(4)?,
        pocket_size: r.get(5)?,
        storage_location: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
        card_count: r.get(9)?,
    })
}

/// Create a binder; returns its id.
pub fn create(conn: &Connection, new: &NewBinder) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO binders \
           (name, description, color, binder_type, pocket_size, \
            storage_location, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![
            new.name,
            new.description,
            new.color,
            new.binder_type,
            new.pocket_size.unwrap_or(9),
            new.storage_location,
            now,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetch one binder.
pub fn get(conn: &Connection, id: i64) -> Result<Option<Binder>> {
    Ok(conn
        .prepare(&format!("SELECT {COLS} FROM binders b WHERE b.id = ?1"))?
        .query_row([id], from_row)
        .optional()?)
}

/// List binders, alphabetically.
pub fn list(conn: &Connection) -> Result<Vec<Binder>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM binders b ORDER BY b.name"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Update editable fields. Returns whether a row changed.
pub fn update(conn: &Connection, id: i64, edit: &BinderEdit) -> Result<bool> {
    let n = conn.execute(
        "UPDATE binders SET \
           name             = COALESCE(?2, name), \
           description      = COALESCE(?3, description), \
           color            = COALESCE(?4, color), \
           binder_type      = COALESCE(?5, binder_type), \
           pocket_size      = COALESCE(?6, pocket_size), \
           storage_location = COALESCE(?7, storage_location), \
           updated_at       = ?8 \
         WHERE id = ?1",
        params![
            id,
            edit.name,
            edit.description,
            edit.color,
            edit.binder_type,
            edit.pocket_size,
            edit.storage_location,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(n > 0)
}

/// Delete a binder. Its cards are un-assigned (collection.binder_id is
/// `ON DELETE SET NULL`), never deleted.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM binders WHERE id = ?1", [id])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        open_shared(&shared).unwrap();
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn crud_round_trip() {
        let (_d, conn) = conn();
        let id = create(
            &conn,
            &NewBinder {
                name: "Trade Binder".into(),
                pocket_size: Some(12),
                ..Default::default()
            },
        )
        .unwrap();

        let b = get(&conn, id).unwrap().unwrap();
        assert_eq!(b.name, "Trade Binder");
        assert_eq!(b.pocket_size, 12);
        assert_eq!(b.card_count, 0);

        assert!(
            update(
                &conn,
                id,
                &BinderEdit {
                    name: Some("Showcase".into()),
                    ..Default::default()
                }
            )
            .unwrap()
        );
        assert_eq!(get(&conn, id).unwrap().unwrap().name, "Showcase");

        assert_eq!(list(&conn).unwrap().len(), 1);
        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
    }
}
