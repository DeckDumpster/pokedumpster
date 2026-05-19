//! Opening and wiring up PokeDumpster's databases.
//!
//! The shared catalog is opened read-write only by `pkdump setup` and the
//! ingest pipelines. A per-user collection database `ATTACH`es the catalog
//! read-only and exposes its tables through `TEMP VIEW`s so queries can join
//! user and catalog data unqualified (PLAN.md §3.1).

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

use crate::error::Result;
use crate::migrations;

/// Open the shared catalog database, creating it if absent, applying any
/// pending migrations. Read-write — for `pkdump setup` and ingest only.
pub fn open_shared(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrations::run_shared_migrations(&mut conn)?;
    Ok(conn)
}

/// `ATTACH` the shared catalog read-only as `shared`, then create a
/// `TEMP VIEW` for every catalog table and view so they are queryable
/// unqualified alongside the user database's own tables.
pub fn attach_shared_readonly(conn: &Connection, shared_path: &Path) -> Result<()> {
    let uri = format!("file:{}?mode=ro", shared_path.display());
    conn.execute("ATTACH DATABASE ?1 AS shared", [uri])?;

    let names: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM shared.sqlite_master \
             WHERE type IN ('table', 'view') \
               AND name NOT LIKE 'sqlite_%' \
               AND name <> 'refinery_schema_history'",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    for name in names {
        conn.execute_batch(&format!(
            "CREATE TEMP VIEW IF NOT EXISTS \"{name}\" \
             AS SELECT * FROM shared.\"{name}\";"
        ))?;
    }
    Ok(())
}

/// Open a per-user collection database — applying any pending user-schema
/// migrations — with the shared catalog attached read-only.
pub fn connect_user(user_path: &Path, shared_path: &Path) -> Result<Connection> {
    if let Some(parent) = user_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut conn = Connection::open(user_path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    migrations::run_user_migrations(&mut conn)?;
    attach_shared_readonly(&conn, shared_path)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    fn seed_shared(path: &Path) {
        let conn = open_shared(path).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
             VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn open_shared_creates_and_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='cards'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn attach_exposes_catalog_and_enforces_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        seed_shared(&shared_path);

        // A fresh in-memory "user" connection with the catalog attached.
        let user = Connection::open_in_memory().unwrap();
        attach_shared_readonly(&user, &shared_path).unwrap();

        // Catalog tables are reachable unqualified via the temp views.
        let name: String = user
            .query_row("SELECT name FROM sets WHERE set_code = 'sv3pt5'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "151");

        // FK-existence helpers see the attached catalog.
        assert!(catalog::card_exists(&user, "sv3pt5-1").unwrap());
        assert!(!catalog::card_exists(&user, "sv3pt5-999").unwrap());
        assert!(!catalog::printing_exists(&user, "sv3pt5-1-normal").unwrap());

        // The attachment is read-only: writing to the catalog fails.
        let write = user.execute(
            "INSERT INTO shared.sets (set_code, name, series) VALUES ('x', 'y', 'z')",
            [],
        );
        assert!(write.is_err(), "shared catalog must be read-only");
    }

    #[test]
    fn connect_user_attaches_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        seed_shared(&shared_path);

        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared_path).unwrap();
        assert!(catalog::card_exists(&conn, "sv3pt5-1").unwrap());
    }

    #[test]
    fn connect_user_has_user_schema_and_enforces_exclusivity() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        seed_shared(&shared_path);
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared_path).unwrap();

        conn.execute(
            "INSERT INTO binders (name, created_at, updated_at) \
             VALUES ('Trade Binder', '2026-05-18', '2026-05-18')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO decks (name, created_at, updated_at) \
             VALUES ('Alice''s deck', '2026-05-18', '2026-05-18')",
            [],
        )
        .unwrap();

        // A card may sit in a binder OR a deck.
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source, binder_id) \
             VALUES ('sv3pt5-1-normal', '2026-05-18', 'manual_id', 1)",
            [],
        )
        .unwrap();

        // ...but not both — the exclusivity CHECK rejects it.
        let both = conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source, binder_id, deck_id) \
             VALUES ('sv3pt5-1-reverse_holo', '2026-05-18', 'manual_id', 1, 1)",
            [],
        );
        assert!(
            both.is_err(),
            "a card cannot be in a binder and a deck at once"
        );
    }
}
