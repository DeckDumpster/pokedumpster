//! Repository for `collection_views` — named, saved filter configs for
//! the collection page. `filters_json` is opaque to the backend: the
//! frontend defines and interprets its shape.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;

const COLS: &str = "id, name, description, filters_json, created_at, updated_at";

/// A saved collection view.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CollectionView {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub filters_json: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Fields for saving a new view.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewView {
    pub name: String,
    pub description: Option<String>,
    pub filters_json: String,
}

/// Editable view fields. A `None` field is left unchanged.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct ViewEdit {
    pub name: Option<String>,
    pub description: Option<String>,
    pub filters_json: Option<String>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<CollectionView> {
    Ok(CollectionView {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        filters_json: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
    })
}

/// Save a new view; returns its id.
pub fn create(conn: &Connection, new: &NewView) -> Result<i64> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO collection_views (name, description, filters_json, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![new.name, new.description, new.filters_json, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetch one view.
pub fn get(conn: &Connection, id: i64) -> Result<Option<CollectionView>> {
    Ok(conn
        .prepare(&format!("SELECT {COLS} FROM collection_views WHERE id = ?1"))?
        .query_row([id], from_row)
        .optional()?)
}

/// List views, alphabetically.
pub fn list(conn: &Connection) -> Result<Vec<CollectionView>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM collection_views ORDER BY name"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Update editable fields. Returns whether a row changed.
pub fn update(conn: &Connection, id: i64, edit: &ViewEdit) -> Result<bool> {
    let n = conn.execute(
        "UPDATE collection_views SET \
           name         = COALESCE(?2, name), \
           description  = COALESCE(?3, description), \
           filters_json = COALESCE(?4, filters_json), \
           updated_at   = ?5 \
         WHERE id = ?1",
        params![
            id,
            edit.name,
            edit.description,
            edit.filters_json,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(n > 0)
}

/// Delete a view.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM collection_views WHERE id = ?1", [id])? > 0)
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
            &NewView {
                name: "Holos only".into(),
                description: None,
                filters_json: r#"{"variant":["holo"]}"#.into(),
            },
        )
        .unwrap();

        let v = get(&conn, id).unwrap().unwrap();
        assert_eq!(v.name, "Holos only");
        assert_eq!(v.filters_json, r#"{"variant":["holo"]}"#);

        assert!(update(
            &conn,
            id,
            &ViewEdit {
                filters_json: Some(r#"{"variant":["holo","reverse_holo"]}"#.into()),
                ..Default::default()
            },
        )
        .unwrap());
        assert_eq!(
            get(&conn, id).unwrap().unwrap().filters_json,
            r#"{"variant":["holo","reverse_holo"]}"#
        );

        assert_eq!(list(&conn).unwrap().len(), 1);
        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
        assert!(!delete(&conn, id).unwrap());
    }
}
