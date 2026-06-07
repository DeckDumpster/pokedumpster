//! Opening and wiring up PokeDumpster's databases.
//!
//! The shared catalog is opened read-write only by `pkdump setup` and the
//! ingest pipelines. A per-user collection database `ATTACH`es the catalog
//! read-only and exposes its tables through `TEMP VIEW`s so queries can join
//! user and catalog data unqualified (PLAN.md §3.1).
//!
//! Schema management: single-instance project (pokedumpster-luo). The full
//! schema lives in `schema_shared.sql` / `schema_user.sql` and is re-applied
//! with `CREATE … IF NOT EXISTS` on every open. No migration history, no
//! refinery — future schema changes edit those files and manually apply
//! the diff to the one prod box.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::backup::Backup;

use crate::error::Result;

const SCHEMA_SHARED: &str = include_str!("schema_shared.sql");
const SCHEMA_USER: &str = include_str!("schema_user.sql");

/// Open the shared catalog database, creating it if absent, and apply the
/// schema (idempotent — every CREATE is IF NOT EXISTS). Read-write — for
/// `pkdump setup` and ingest only.
///
/// PRAGMAs tuned for the variant-expansion write workload, which opens
/// ~20k per-card transactions: WAL keeps writes sequential, synchronous
/// = NORMAL drops the per-commit fsync (still crash-safe in WAL mode),
/// and a 64MB page cache keeps the printings + indices hot through the
/// full expansion pass. Without these, throughput collapses ~3× once
/// the table exceeds the default ~2MB cache (pokedumpster-rqr).
///
/// After schema init, reconciles every shipped seed file (variants,
/// (group, sub_type) → variant map, bundles) so a freshly-opened DB is
/// always ready for FK-referencing inserts. Cheap and idempotent on the
/// existing prod DB. See pokedumpster-luo.
pub fn open_shared(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL; \
         PRAGMA synchronous = NORMAL; \
         PRAGMA cache_size = -65536; \
         PRAGMA foreign_keys = ON;",
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA_SHARED)?;
    // Reconcile shipped seeds — variants must run first (sub_type_map
    // FKs into it). All three are idempotent upserts.
    crate::variants::reconcile(&mut conn)?;
    crate::sub_type_map::reconcile(&mut conn)?;
    crate::bundles::reconcile(&mut conn)?;
    Ok(conn)
}

/// `ATTACH` the shared catalog read-only as `shared`, then create a
/// `TEMP VIEW` for every catalog table and view so they are queryable
/// unqualified alongside the user database's own tables.
pub fn attach_shared_readonly(conn: &Connection, shared_path: &Path) -> Result<()> {
    let uri = format!("file:{}?mode=ro", shared_path.display());
    conn.execute("ATTACH DATABASE ?1 AS shared", [uri])?;

    // Skip sqlite_* internals and the legacy refinery_schema_history
    // table left behind on the prod DB by the pre-luo migration system.
    // It's harmless dead weight; new installs never get it.
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

/// Open a per-user collection database — applying the user schema — with
/// the shared catalog attached read-only.
pub fn connect_user(user_path: &Path, shared_path: &Path) -> Result<Connection> {
    if let Some(parent) = user_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(user_path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(SCHEMA_USER)?;
    attach_shared_readonly(&conn, shared_path)?;
    Ok(conn)
}

/// Apply the user schema to an arbitrary connection. Used by tests that
/// open an in-memory user DB without going through `connect_user`.
pub fn init_user_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_USER)?;
    Ok(())
}

/// Snapshot a live SQLite database to `dest`, overwriting it.
///
/// WAL-correct: uses SQLite's online backup API, which captures a
/// transactionally-consistent view — including any committed WAL frames —
/// even while the server holds the database open. Backs the UI test
/// harness's per-test isolation, replacing the old in-container
/// `python3 sqlite3.backup()` (pokedumpster-0g3).
pub fn snapshot_db(live: &Path, dest: &Path) -> Result<()> {
    // A leftover -wal/-shm beside a stale snapshot would shadow the bytes we
    // copy in; start from a clean destination.
    remove_db_files(dest);
    let src = Connection::open(live)?;
    src.busy_timeout(Duration::from_secs(10))?;
    let mut dst = Connection::open(dest)?;
    let backup = Backup::new(&src, &mut dst)?;
    backup.run_to_completion(256, Duration::from_millis(50), None)?;
    Ok(())
}

/// Restore a live SQLite database from a snapshot taken by [`snapshot_db`].
///
/// WAL-correct: copies the snapshot *into* the live database through the
/// online backup API, committing every page as a fresh transaction. This is
/// what makes it safe while the server holds the database open — a plain
/// `cp` of the main file leaves the live `-wal`/`-shm` in place, so the next
/// read replays a prior test's frames on top of the restored bytes and sees
/// the mutated state (pokedumpster-lxm).
pub fn restore_db(snapshot: &Path, live: &Path) -> Result<()> {
    let src = Connection::open(snapshot)?;
    let mut dst = Connection::open(live)?;
    dst.busy_timeout(Duration::from_secs(10))?;
    let backup = Backup::new(&src, &mut dst)?;
    backup.run_to_completion(256, Duration::from_millis(50), None)?;
    Ok(())
}

/// Best-effort removal of a SQLite database file and its WAL sidecars.
fn remove_db_files(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut p = path.as_os_str().to_owned();
        p.push(suffix);
        let _ = std::fs::remove_file(p);
    }
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
    fn open_shared_creates_schema() {
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
        // Re-opening is idempotent.
        let conn2 = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        let n2: i64 = conn2
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='cards'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n2, 1);
    }

    #[test]
    fn attach_exposes_catalog_and_enforces_readonly() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        seed_shared(&shared_path);

        // A fresh in-memory "user" connection with the catalog attached.
        // Apply the user schema too so the FK-existence helpers can see
        // the user_printings table (the "Missing Variant" escape hatch
        // is one of the FK targets `printing_exists` checks).
        let user = Connection::open_in_memory().unwrap();
        init_user_schema(&user).unwrap();
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
    fn restore_is_wal_correct_with_live_connection() {
        // Reproduces pokedumpster-lxm: the server holds a long-lived WAL
        // connection; a prior test's write lands in the WAL. A WAL-unaware
        // restore (cp of the main file only) leaves that frame in place, so
        // the next read still sees the mutation. WAL-correct restore must
        // overwrite it.
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("collection.sqlite");

        // The "server" — a persistent connection in WAL mode.
        let server = Connection::open(&live).unwrap();
        server
            .execute_batch(
                "PRAGMA journal_mode = WAL; \
                 CREATE TABLE t (name TEXT PRIMARY KEY, copies INTEGER); \
                 INSERT INTO t VALUES ('Blastoise', 1);",
            )
            .unwrap();

        // Snapshot the clean state (Blastoise = 1).
        let bak = dir.path().join("collection.sqlite.bak");
        snapshot_db(&live, &bak).unwrap();

        // A test mutates through the live connection — the write lands in the
        // WAL, not (yet) the main database file.
        server
            .execute("UPDATE t SET copies = 2 WHERE name = 'Blastoise'", [])
            .unwrap();
        let mutated: i64 = server
            .query_row("SELECT copies FROM t WHERE name = 'Blastoise'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(mutated, 2);

        // Restore, then read back through the *same* live connection.
        restore_db(&bak, &live).unwrap();
        let restored: i64 = server
            .query_row("SELECT copies FROM t WHERE name = 'Blastoise'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(
            restored, 1,
            "live connection must see the restored snapshot, not the WAL mutation"
        );
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
