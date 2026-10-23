//! Repository for the `decks` table — physical decks built from the
//! collection. Decks carry a 3-state lifecycle: idea → ready → built.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::Result;

const COLS: &str = "d.id, d.name, d.description, d.format, d.owner, d.state, \
     d.sleeve_color, d.storage_location, d.notes, d.created_at, d.updated_at, \
     d.group_id, (SELECT count(*) FROM collection c WHERE c.deck_id = d.id)";

/// A deck, with the count of cards assigned to it.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Deck {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub format: Option<String>,
    pub owner: Option<String>,
    /// `idea` | `ready` | `built`.
    pub state: String,
    pub sleeve_color: Option<String>,
    pub storage_location: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// The deck group this deck belongs to, if any.
    #[ts(type = "number | null")]
    pub group_id: Option<i64>,
    #[ts(type = "number")]
    pub card_count: i64,
}

/// Fields for creating a deck.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewDeck {
    pub name: String,
    pub description: Option<String>,
    pub format: Option<String>,
    pub owner: Option<String>,
    pub state: Option<String>,
    pub sleeve_color: Option<String>,
    pub storage_location: Option<String>,
    pub notes: Option<String>,
    #[ts(type = "number | null")]
    pub group_id: Option<i64>,
}

/// Editable deck fields. A `None` field is left unchanged.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct DeckEdit {
    pub name: Option<String>,
    pub description: Option<String>,
    pub format: Option<String>,
    pub owner: Option<String>,
    pub state: Option<String>,
    pub sleeve_color: Option<String>,
    pub storage_location: Option<String>,
    pub notes: Option<String>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Deck> {
    Ok(Deck {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        format: r.get(3)?,
        owner: r.get(4)?,
        state: r.get(5)?,
        sleeve_color: r.get(6)?,
        storage_location: r.get(7)?,
        notes: r.get(8)?,
        created_at: r.get(9)?,
        updated_at: r.get(10)?,
        group_id: r.get(11)?,
        card_count: r.get(12)?,
    })
}

/// Create a deck; returns its id.
pub fn create(conn: &Connection, new: &NewDeck) -> Result<i64> {
    let now = crate::clock::now_rfc3339();
    conn.execute(
        "INSERT INTO decks \
           (name, description, format, owner, state, sleeve_color, \
            storage_location, notes, group_id, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?10, ?9, ?9)",
        params![
            new.name,
            new.description,
            new.format,
            new.owner,
            new.state.as_deref().unwrap_or("idea"),
            new.sleeve_color,
            new.storage_location,
            new.notes,
            now,
            new.group_id,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetch one deck.
pub fn get(conn: &Connection, id: i64) -> Result<Option<Deck>> {
    Ok(conn
        .prepare(&format!("SELECT {COLS} FROM decks d WHERE d.id = ?1"))?
        .query_row([id], from_row)
        .optional()?)
}

/// List decks in natural name order.
pub fn list(conn: &Connection) -> Result<Vec<Deck>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM decks d ORDER BY d.id"))?;
    let rows = stmt.query_map([], from_row)?;
    let mut all: Vec<_> = rows.collect::<rusqlite::Result<_>>()?;
    // Natural order ("Pack 2" before "Pack 10"); the stable sort keeps id order on ties.
    all.sort_by(|a, b| pkdump_core::natural::natural_cmp(&a.name, &b.name));
    Ok(all)
}

/// Update editable fields. Returns whether a row changed.
pub fn update(conn: &Connection, id: i64, edit: &DeckEdit) -> Result<bool> {
    let n = conn.execute(
        "UPDATE decks SET \
           name             = COALESCE(?2, name), \
           description      = COALESCE(?3, description), \
           format           = COALESCE(?4, format), \
           owner            = COALESCE(?5, owner), \
           state            = COALESCE(?6, state), \
           sleeve_color     = COALESCE(?7, sleeve_color), \
           storage_location = COALESCE(?8, storage_location), \
           notes            = COALESCE(?9, notes), \
           updated_at       = ?10 \
         WHERE id = ?1",
        params![
            id,
            edit.name,
            edit.description,
            edit.format,
            edit.owner,
            edit.state,
            edit.sleeve_color,
            edit.storage_location,
            edit.notes,
            crate::clock::now_rfc3339(),
        ],
    )?;
    Ok(n > 0)
}

/// Delete a deck. Its cards are un-assigned (collection.deck_id is
/// `ON DELETE SET NULL`), never deleted.
pub fn delete(conn: &Connection, id: i64) -> Result<bool> {
    // `movement_log` keeps the move but forgets the deck, or its FK would refuse the delete.
    conn.execute(
        "UPDATE movement_log SET from_deck_id = NULL WHERE from_deck_id = ?1",
        [id],
    )?;
    conn.execute(
        "UPDATE movement_log SET to_deck_id = NULL WHERE to_deck_id = ?1",
        [id],
    )?;
    Ok(conn.execute("DELETE FROM decks WHERE id = ?1", [id])? > 0)
}

/// Move a deck into a group, or out of any (`None`). Returns whether the deck exists.
pub fn set_group(conn: &Connection, id: i64, group_id: Option<i64>) -> Result<bool> {
    if let Some(g) = group_id
        && crate::deck_groups::get(conn, g)?.is_none()
    {
        return Err(crate::DbError::NotFound(format!("deck group {g}")));
    }
    let n = conn.execute(
        "UPDATE decks SET group_id = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, group_id, crate::clock::now_rfc3339()],
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
        open_shared(&shared).unwrap();
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn crud_round_trip() {
        let (_d, conn) = conn();
        let id = create(
            &conn,
            &NewDeck {
                name: "Alice's Charizard deck".into(),
                owner: Some("Alice".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let d = get(&conn, id).unwrap().unwrap();
        assert_eq!(d.owner.as_deref(), Some("Alice"));
        assert_eq!(d.state, "idea"); // default lifecycle state
        assert_eq!(d.card_count, 0);

        assert!(
            update(
                &conn,
                id,
                &DeckEdit {
                    state: Some("built".into()),
                    ..Default::default()
                }
            )
            .unwrap()
        );
        assert_eq!(get(&conn, id).unwrap().unwrap().state, "built");

        assert!(delete(&conn, id).unwrap());
        assert!(get(&conn, id).unwrap().is_none());
    }

    #[test]
    fn rejects_invalid_state() {
        let (_d, conn) = conn();
        let err = create(
            &conn,
            &NewDeck {
                name: "Bad".into(),
                state: Some("nonsense".into()),
                ..Default::default()
            },
        );
        assert!(
            err.is_err(),
            "the state CHECK constraint should reject this"
        );
    }
}

#[cfg(test)]
mod natural_order {
    use super::*;
    use crate::{connect_user, open_shared};

    #[test]
    fn decks_list_in_natural_order() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        open_shared(&shared).unwrap();
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        for n in ["Pack 1", "Pack 10", "Pack 2"] {
            create(
                &conn,
                &NewDeck {
                    name: n.into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let names: Vec<_> = list(&conn).unwrap().into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Pack 1", "Pack 2", "Pack 10"]);
    }
}
