//! Repository for the `deck_groups` table — a first-class container for related
//! decks (a cube's packs), so they are one managed unit. A deck belongs to at
//! most one group (`decks.group_id`); a group's name is unique per tenant.

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{DbError, Result};

const COLS: &str = "g.id, g.name, g.description, g.format, g.created_at, g.updated_at, \
     (SELECT count(*) FROM decks d WHERE d.group_id = g.id), \
     (SELECT count(*) FROM collection c JOIN decks d ON d.id = c.deck_id \
        WHERE d.group_id = g.id)";

/// A deck group with its totals.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct DeckGroup {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub format: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Decks in the group.
    #[ts(type = "number")]
    pub deck_count: i64,
    /// Cards assigned to those decks.
    #[ts(type = "number")]
    pub card_count: i64,
}

/// Fields for creating a deck group.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewDeckGroup {
    pub name: String,
    pub description: Option<String>,
    pub format: Option<String>,
}

/// Editable deck-group fields. A `None` field is left unchanged.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct DeckGroupEdit {
    pub name: Option<String>,
    pub description: Option<String>,
    pub format: Option<String>,
}

/// What deleting a group does to its decks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DeleteMode {
    /// Keep the decks, now ungrouped.
    Ungroup,
    /// Delete the decks with the group; their cards go back to the collection.
    Decks,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<DeckGroup> {
    Ok(DeckGroup {
        id: r.get(0)?,
        name: r.get(1)?,
        description: r.get(2)?,
        format: r.get(3)?,
        created_at: r.get(4)?,
        updated_at: r.get(5)?,
        deck_count: r.get(6)?,
        card_count: r.get(7)?,
    })
}

fn check_name(conn: &Connection, name: &str, except: Option<i64>) -> Result<()> {
    if name.trim().is_empty() {
        return Err(DbError::Invalid("a deck group needs a name".into()));
    }
    let taken: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM deck_groups WHERE name = ?1 AND id IS NOT ?2)",
        params![name, except],
        |r| r.get(0),
    )?;
    if taken {
        return Err(DbError::Conflict(format!(
            "a deck group named \"{name}\" already exists"
        )));
    }
    Ok(())
}

/// Create a deck group; returns its id.
pub fn create(conn: &Connection, new: &NewDeckGroup) -> Result<i64> {
    check_name(conn, &new.name, None)?;
    let now = crate::clock::now_rfc3339();
    conn.execute(
        "INSERT INTO deck_groups (name, description, format, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?4)",
        params![new.name, new.description, new.format, now],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Fetch one deck group.
pub fn get(conn: &Connection, id: i64) -> Result<Option<DeckGroup>> {
    Ok(conn
        .prepare(&format!("SELECT {COLS} FROM deck_groups g WHERE g.id = ?1"))?
        .query_row([id], from_row)
        .optional()?)
}

/// List deck groups in natural name order ("Pack 2" before "Pack 10").
pub fn list(conn: &Connection) -> Result<Vec<DeckGroup>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM deck_groups g ORDER BY g.id"))?;
    let mut all: Vec<_> = stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    all.sort_by(|a, b| pkdump_core::natural::natural_cmp(&a.name, &b.name));
    Ok(all)
}

/// The decks of a group, in natural name order.
pub fn decks_of(conn: &Connection, id: i64) -> Result<Vec<crate::decks::Deck>> {
    let mut all: Vec<_> = crate::decks::list(conn)?
        .into_iter()
        .filter(|d| d.group_id == Some(id))
        .collect();
    all.sort_by(|a, b| pkdump_core::natural::natural_cmp(&a.name, &b.name));
    Ok(all)
}

/// Update editable fields. Returns whether a row changed.
pub fn update(conn: &Connection, id: i64, edit: &DeckGroupEdit) -> Result<bool> {
    if let Some(name) = &edit.name {
        check_name(conn, name, Some(id))?;
    }
    let n = conn.execute(
        "UPDATE deck_groups SET \
           name        = COALESCE(?2, name), \
           description = COALESCE(?3, description), \
           format      = COALESCE(?4, format), \
           updated_at  = ?5 \
         WHERE id = ?1",
        params![
            id,
            edit.name,
            edit.description,
            edit.format,
            crate::clock::now_rfc3339()
        ],
    )?;
    Ok(n > 0)
}

/// Release every deck in the group: each assigned card returns to the loose
/// collection (one `movement_log` entry apiece). The decks stay. Returns the
/// number of cards released.
pub fn release_all(conn: &mut Connection, id: i64) -> Result<usize> {
    if get(conn, id)?.is_none() {
        return Err(DbError::NotFound(format!("deck group {id}")));
    }
    let tx = conn.transaction()?;
    let now = crate::clock::now_rfc3339();
    tx.execute(
        "INSERT INTO movement_log (collection_id, from_deck_id, to_deck_id, changed_at, note) \
         SELECT c.id, c.deck_id, NULL, ?2, 'deck group released' FROM collection c \
           JOIN decks d ON d.id = c.deck_id WHERE d.group_id = ?1",
        params![id, now],
    )?;
    let n = tx.execute(
        "UPDATE collection SET deck_id = NULL \
         WHERE deck_id IN (SELECT id FROM decks WHERE group_id = ?1)",
        [id],
    )?;
    tx.commit()?;
    Ok(n)
}

/// Delete a group. `Ungroup` keeps its decks; `Decks` deletes them too (their
/// cards go back to the collection, never deleted). Returns whether it existed.
pub fn delete(conn: &mut Connection, id: i64, mode: DeleteMode) -> Result<bool> {
    if get(conn, id)?.is_none() {
        return Ok(false);
    }
    if mode == DeleteMode::Decks {
        release_all(conn, id)?;
        let ids: Vec<i64> = conn
            .prepare("SELECT id FROM decks WHERE group_id = ?1")?
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for d in ids {
            crate::decks::delete(conn, d)?;
        }
    }
    Ok(conn.execute("DELETE FROM deck_groups WHERE id = ?1", [id])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decks::{self, NewDeck};
    use crate::{connect_user, open_shared};

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        open_shared(&shared).unwrap();
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn group(c: &Connection, name: &str) -> i64 {
        create(
            c,
            &NewDeckGroup {
                name: name.into(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn deck(c: &Connection, name: &str, g: Option<i64>) -> i64 {
        decks::create(
            c,
            &NewDeck {
                name: name.into(),
                group_id: g,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[test]
    fn crud_round_trip_and_unique_names() {
        let (_d, c) = conn();
        let g = group(&c, "Cube");
        assert!(matches!(
            create(
                &c,
                &NewDeckGroup {
                    name: "Cube".into(),
                    ..Default::default()
                }
            ),
            Err(DbError::Conflict(_))
        ));
        let other = group(&c, "Other");
        assert!(matches!(
            update(
                &c,
                other,
                &DeckGroupEdit {
                    name: Some("Cube".into()),
                    ..Default::default()
                }
            ),
            Err(DbError::Conflict(_))
        ));
        assert!(
            update(
                &c,
                g,
                &DeckGroupEdit {
                    name: Some("Cube v2".into()),
                    format: Some("cube".into()),
                    ..Default::default()
                }
            )
            .unwrap()
        );
        let got = get(&c, g).unwrap().unwrap();
        assert_eq!(
            (got.name.as_str(), got.format.as_deref()),
            ("Cube v2", Some("cube"))
        );
        assert!(matches!(
            create(
                &c,
                &NewDeckGroup {
                    name: "  ".into(),
                    ..Default::default()
                }
            ),
            Err(DbError::Invalid(_))
        ));
    }

    #[test]
    fn groups_and_their_decks_list_naturally() {
        let (_d, c) = conn();
        for n in ["Pack 1", "Pack 10", "Pack 2"] {
            group(&c, n);
        }
        let names: Vec<_> = list(&c).unwrap().into_iter().map(|g| g.name).collect();
        assert_eq!(names, ["Pack 1", "Pack 2", "Pack 10"]);
        let g = group(&c, "G");
        for n in ["Deck 10", "Deck 2"] {
            deck(&c, n, Some(g));
        }
        let names: Vec<_> = decks_of(&c, g)
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(names, ["Deck 2", "Deck 10"]);
    }

    #[test]
    fn a_deck_moves_into_and_out_of_a_group() {
        let (_d, c) = conn();
        let g = group(&c, "G");
        let d = deck(&c, "D", None);
        assert!(decks::set_group(&c, d, Some(g)).unwrap());
        assert_eq!(decks::get(&c, d).unwrap().unwrap().group_id, Some(g));
        assert_eq!(get(&c, g).unwrap().unwrap().deck_count, 1);
        assert!(decks::set_group(&c, d, None).unwrap());
        assert_eq!(decks::get(&c, d).unwrap().unwrap().group_id, None);
        assert!(decks::set_group(&c, d, Some(999)).is_err());
    }

    /// A collection whose catalog holds one printing, with `n` copies in each given deck.
    fn with_cards() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('s', 'S', 'X')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('s-1', 's', '1', 1, 'Bulb')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) VALUES ('s-1-normal', 's-1', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn add_copies(c: &mut Connection, deck_id: i64, n: usize) {
        for _ in 0..n {
            let id = crate::collection::add(
                c,
                &crate::collection::NewCopy {
                    printing_id: "s-1-normal".into(),
                    source: "manual_id".into(),
                    ..Default::default()
                },
            )
            .unwrap();
            crate::collection::move_to(c, id, None, Some(deck_id), None).unwrap();
        }
    }

    fn in_decks(c: &Connection) -> i64 {
        c.query_row(
            "SELECT count(*) FROM collection WHERE deck_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn total(c: &Connection) -> i64 {
        c.query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn release_all_returns_every_card_and_keeps_the_decks() {
        let (_d, mut c) = with_cards();
        let g = group(&c, "G");
        let a = deck(&c, "A", Some(g));
        let b = deck(&c, "B", Some(g));
        let loose = deck(&c, "Loose", None);
        add_copies(&mut c, a, 2);
        add_copies(&mut c, b, 1);
        add_copies(&mut c, loose, 1);
        assert_eq!(get(&c, g).unwrap().unwrap().card_count, 3);
        assert_eq!(release_all(&mut c, g).unwrap(), 3);
        assert_eq!(get(&c, g).unwrap().unwrap().card_count, 0);
        assert_eq!(get(&c, g).unwrap().unwrap().deck_count, 2);
        assert_eq!(in_decks(&c), 1, "the ungrouped deck's card is untouched");
        assert_eq!(total(&c), 4);
    }

    #[test]
    fn delete_ungroup_keeps_decks_and_cards() {
        let (_d, mut c) = with_cards();
        let g = group(&c, "G");
        let a = deck(&c, "A", Some(g));
        add_copies(&mut c, a, 2);
        assert!(delete(&mut c, g, DeleteMode::Ungroup).unwrap());
        assert!(get(&c, g).unwrap().is_none());
        assert_eq!(decks::get(&c, a).unwrap().unwrap().group_id, None);
        assert_eq!(in_decks(&c), 2);
    }

    #[test]
    fn delete_with_decks_deletes_them_and_releases_their_cards() {
        let (_d, mut c) = with_cards();
        let g = group(&c, "G");
        let a = deck(&c, "A", Some(g));
        let b = deck(&c, "B", Some(g));
        let loose = deck(&c, "Loose", None);
        add_copies(&mut c, a, 2);
        add_copies(&mut c, b, 1);
        assert!(delete(&mut c, g, DeleteMode::Decks).unwrap());
        assert!(decks::get(&c, a).unwrap().is_none());
        assert!(decks::get(&c, b).unwrap().is_none());
        assert!(decks::get(&c, loose).unwrap().is_some());
        assert_eq!(total(&c), 3, "cards are never deleted");
        assert_eq!(in_decks(&c), 0);
        assert!(!delete(&mut c, g, DeleteMode::Ungroup).unwrap());
    }
}
