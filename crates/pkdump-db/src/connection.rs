//! Opening and wiring up PokeDumpster's databases.
//!
//! The shared catalog is opened read-write only by `pkdump setup` and the
//! ingest pipelines. A per-user collection database `ATTACH`es the catalog
//! read-only and exposes its tables through `TEMP VIEW`s so queries can join
//! user and catalog data unqualified (PLAN.md §3.1).
//!
//! Schema management: the full schema lives in `schema_shared.sql` /
//! `schema_user.sql` and is re-applied with `CREATE … IF NOT EXISTS` on
//! every open. No migration history, no refinery — additive change travels
//! by idempotent re-application (pokedumpster-luo).
//!
//! What that cannot express — a change that transforms or drops — is gated
//! instead of applied: every database carries its schema version in
//! `PRAGMA user_version`, and a file written by a newer build is REFUSED
//! rather than opened. See [`crate::schema_version`] for the three
//! outcomes and why the refusal is what makes rollback safe (pd-ja38).

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::backup::Backup;

use crate::error::Result;
use crate::schema_version::{self, Database};

const SCHEMA_SHARED: &str = include_str!("schema_shared.sql");
const SCHEMA_USER: &str = include_str!("schema_user.sql");
const SCHEMA_REGISTRY: &str = include_str!("schema_registry.sql");

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
/// (group, sub_type) → variant map, bundles, set-name aliases) so a
/// freshly-opened DB is always ready for FK-referencing inserts. Cheap and
/// idempotent on the existing prod DB. See pokedumpster-luo.
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
    // Before a single statement of schema runs: a catalog written by a newer
    // build is refused, not migrated backwards into.
    schema_version::gate(&conn, Database::Shared)?;
    conn.execute_batch(SCHEMA_SHARED)?;
    add_missing_columns(&conn)?;
    // Reconcile shipped seeds — variants must run first (sub_type_map
    // FKs into it). All three are idempotent upserts.
    crate::variants::reconcile(&mut conn)?;
    crate::sub_type_map::reconcile(&mut conn)?;
    crate::bundles::reconcile(&mut conn)?;
    crate::set_aliases::reconcile(&mut conn)?;
    // Last of the seeds: its rows FK into `printings`, so it writes nothing
    // until the catalog has been ingested. `pkdump-lake-derive shared` re-opens
    // the catalog after every derivation, which is where it lands for real.
    crate::catalog_prices::reconcile(&mut conn)?;
    // Stamped last: the file claims this shape only once it has it.
    schema_version::stamp(&conn, Database::Shared)?;
    Ok(conn)
}

/// Open the shared catalog **read-only**.
///
/// No schema application, no seed reconciliation, no version stamp — and no
/// writes, enforced by SQLite rather than by review. `SQLITE_OPEN_READ_ONLY`
/// makes an attempted write an error at the connection, so "this caller does
/// not write the catalog" is a property of the handle rather than a claim
/// about the code that holds it.
///
/// That is the whole reason it exists (pd-lunn). Since the derivation left
/// `pkdump data refresh`, the refresh's only interest in the catalog is one
/// question — which sets it already has, so it knows which cards to fetch —
/// and the acceptance criterion for that change is that the command writes no
/// catalog table at all. A read-only handle cannot, including from code
/// nobody has written yet.
///
/// The file must already exist: creating it is `pkdump setup`'s job, and a
/// refresh that quietly built an empty catalog would land every set's cards
/// every night rather than the handful that are new.
pub fn open_shared_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // The same refusal `open_shared` makes, in the same place: a catalog
    // written by a newer build is not one this build may read rows out of and
    // act on. Reading is what this handle is for, so the gate is the whole of
    // the check — there is nothing here to stamp.
    schema_version::gate(&conn, Database::Shared)?;
    Ok(conn)
}

/// Columns added to `schema_shared.sql` after the prod database was
/// built. `CREATE TABLE IF NOT EXISTS` is a no-op against an existing
/// table, so a new column reaches an existing catalog only through
/// `ALTER TABLE`. This keeps that convergence in the same place as the
/// schema instead of in a runbook step: the declaration in
/// `schema_shared.sql` stays the single description of the shape, and a
/// catalog built before the column simply grows it on the next open.
///
/// Nullable, defaultless columns only — anything needing a backfill is a
/// real migration and belongs in a one-off command.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[(
    "sets",
    "discovered_from_group_id",
    "ALTER TABLE sets ADD COLUMN discovered_from_group_id INTEGER",
)];

/// The same convergence for `schema_user.sql`. A collection created between
/// pd-5m54 and pd-385w already carries `ownership_outbox`, so the amended
/// `CREATE TABLE IF NOT EXISTS` above does nothing to it and the provenance
/// column arrives only here.
///
/// This one is not defaultless, and that is the point rather than an
/// exception to the rule above: every event such a collection already holds
/// was written by a trigger, so `DEFAULT 'trigger'` states what is true of
/// all of them. There is no backfill to do — which is what makes it
/// expressible as an `ALTER` at all.
///
/// **No `user_version` bump.** The gate exists to stop an older binary that
/// would get a collection *wrong*; one that has never heard of `source`
/// reads the outbox exactly as it did before and writes events a newer
/// build labels correctly by default. Refusing to open would be a rollback
/// broken for a column that costs nothing to ignore — the same reasoning
/// `schema_user.sql` records for dropping `refinery_schema_history`.
const USER_ADDED_COLUMNS: &[(&str, &str, &str)] = &[(
    "ownership_outbox",
    "source",
    "ALTER TABLE ownership_outbox ADD COLUMN source TEXT NOT NULL \
     DEFAULT 'trigger' CHECK (source IN ('trigger', 'backfill', 'redrive'))",
)];

fn add_missing_columns(conn: &Connection) -> Result<()> {
    add_columns(conn, ADDED_COLUMNS)
}

fn add_missing_user_columns(conn: &Connection) -> Result<()> {
    add_columns(conn, USER_ADDED_COLUMNS)
}

fn add_columns(conn: &Connection, columns: &[(&str, &str, &str)]) -> Result<()> {
    for (table, column, ddl) in columns {
        let present: bool = conn
            .prepare(&format!(
                "SELECT 1 FROM pragma_table_info('{table}') WHERE name = ?1"
            ))?
            .exists([column])?;
        if !present {
            conn.execute_batch(ddl)?;
        }
    }
    Ok(())
}

/// `ATTACH` the shared catalog read-only as `shared`, then create a
/// `TEMP VIEW` for every catalog table and view so they are queryable
/// unqualified alongside the user database's own tables.
///
/// The catalog is version-gated here too. This is the path the *server*
/// reaches it by — `open_shared` is `pkdump setup` / ingest — so without a
/// check here a catalog from a newer build would be joined against
/// silently on every request.
///
/// A catalog name that the collection itself already declares gets **no
/// view**. SQLite resolves an unqualified name in `temp` before `main`, so a
/// TEMP VIEW would not sit beside the collection's own table — it would
/// shade it, and every join would silently read the catalog's rows instead.
/// That is not hypothetical: `gate_attached` deliberately accepts a catalog
/// that is *behind* this build, so a `shared.sqlite` that has not been opened
/// read-write since `conditions` moved into the collection (pd-s4c2) still
/// physically holds the old table. The collection's own tables win; the
/// catalog fills in around them.
pub fn attach_shared_readonly(conn: &Connection, shared_path: &Path) -> Result<()> {
    let uri = format!("file:{}?mode=ro", shared_path.display());
    conn.execute("ATTACH DATABASE ?1 AS shared", [uri])?;
    schema_version::gate_attached(conn, Database::Shared, "shared")?;

    // Everything the catalog declares, minus SQLite's own internals and
    // anything `main` already owns. Nothing else is named here: a table the
    // catalog should not have is dropped by `schema_shared.sql`, not skipped
    // by every reader of it (pd-yj40).
    let names: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT name FROM shared.sqlite_master \
             WHERE type IN ('table', 'view') \
               AND name NOT LIKE 'sqlite_%' \
               AND name NOT IN (SELECT name FROM main.sqlite_master \
                                WHERE type IN ('table', 'view'))",
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

/// Open a per-user collection database — applying the user schema — without
/// the shared catalog. For work that touches only user tables (the JSON
/// backup), which must also run on a box where `pkdump setup` has not built
/// a catalog yet.
///
/// Seeds the collection's `conditions` with the five defaults if they are
/// absent (pd-s4c2). One mechanism covers both cases the move created: a
/// brand-new collection is born with its multipliers, and one written before
/// the table lived here grows them on its next open. Insert-if-absent, so an
/// already-seeded collection is not written to at all.
pub fn open_user(user_path: &Path) -> Result<Connection> {
    if let Some(parent) = user_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(user_path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    schema_version::gate(&conn, Database::User)?;
    conn.execute_batch(SCHEMA_USER)?;
    add_missing_user_columns(&conn)?;
    crate::conditions::seed_defaults(&conn)?;
    schema_version::stamp(&conn, Database::User)?;
    Ok(conn)
}

/// Open the user registry database, creating it if absent, and apply the
/// registry schema (idempotent, like the other two).
///
/// Never attaches the catalog: the registry answers one question — which
/// database file belongs to this handle — and joins nothing. WAL so a
/// resolver reading it is not blocked by a `pkdump tenant create` writing
/// it.
///
/// Gated and stamped exactly as [`open_user`] is, and for a sharper reason:
/// this is the file that says whose database is whose. A build that did not
/// understand its shape and wrote to it anyway would not corrupt a
/// collection — it would corrupt the map, and an unattributable collection is
/// the failure this whole layout exists to prevent (`deploy/TENANTS.md`).
/// Every registry in existence is version 0, so the adoption path is the one
/// that actually runs (pd-r60h).
pub fn open_registry(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    schema_version::gate(&conn, Database::Registry)?;
    conn.execute_batch(SCHEMA_REGISTRY)?;
    schema_version::stamp(&conn, Database::Registry)?;
    Ok(conn)
}

/// Open a per-user collection database — applying the user schema — with
/// the shared catalog attached read-only.
pub fn connect_user(user_path: &Path, shared_path: &Path) -> Result<Connection> {
    let conn = open_user(user_path)?;
    attach_shared_readonly(&conn, shared_path)?;
    Ok(conn)
}

/// Apply the user schema to an arbitrary connection. Used by tests that
/// open an in-memory user DB without going through `connect_user`.
///
/// Gates and stamps exactly as [`open_user`] does, so a test connection is
/// not a second, laxer way into the user schema.
pub fn init_user_schema(conn: &Connection) -> Result<()> {
    schema_version::gate(conn, Database::User)?;
    conn.execute_batch(SCHEMA_USER)?;
    add_missing_user_columns(conn)?;
    crate::conditions::seed_defaults(conn)?;
    schema_version::stamp(conn, Database::User)?;
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

    /// Build a collection database the way the binary that predates the
    /// gate did: schema applied straight, no `user_version` written, rows in
    /// it. This is the shape of every database on disk today, prod's
    /// included — and the shape that took prod down on 2026-08-08, when
    /// every verification was fresh-install shaped and nobody started the
    /// new binary against a volume the old one made.
    fn unversioned_collection(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .unwrap();
        conn.execute_batch(SCHEMA_USER).unwrap();
        conn.execute(
            "INSERT INTO binders (name, created_at, updated_at) \
             VALUES ('Trade Binder', '2026-08-08', '2026-08-08')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source, binder_id) \
             VALUES ('sv3pt5-1-normal', '2026-08-08', 'manual_id', 1)",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(conn);
        assert_eq!(
            file_user_version(path),
            0,
            "the fixture must be genuinely unversioned"
        );
    }

    /// `user_version` straight out of the file header (bytes 60..64,
    /// big-endian), read without opening the database — so the assertion
    /// cannot be satisfied by the very code under test.
    fn file_user_version(path: &Path) -> u32 {
        let bytes = std::fs::read(path).unwrap();
        u32::from_be_bytes(bytes[60..64].try_into().unwrap())
    }

    /// The file change counter (header bytes 24..28), which SQLite bumps on
    /// every write transaction. An unchanged counter is proof that an open
    /// touched nothing.
    fn file_change_counter(path: &Path) -> u32 {
        let bytes = std::fs::read(path).unwrap();
        u32::from_be_bytes(bytes[24..28].try_into().unwrap())
    }

    /// The adoption path, which is the release-blocking one: every database
    /// in existence is version 0, so if this is wrong, prod does not start.
    #[test]
    fn an_unversioned_collection_is_adopted_in_place_with_its_rows() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        unversioned_collection(&path);
        let before = std::fs::metadata(&path).unwrap().ino();

        let conn = open_user(&path).unwrap();

        let rows: i64 = conn
            .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "adoption must not lose the collection");
        let binder: String = conn
            .query_row("SELECT name FROM binders WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(binder, "Trade Binder");
        drop(conn);

        assert_eq!(
            file_user_version(&path),
            Database::User.version() as u32,
            "the adopted database must carry the version afterwards"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            before,
            "adoption must happen in place — the file must not be recreated"
        );
    }

    #[test]
    fn an_unversioned_catalog_is_adopted_and_keeps_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.sqlite");
        seed_shared(&path);
        // Put it back the way the pre-gate binary left it.
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("PRAGMA user_version = 0; PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
        }
        assert_eq!(file_user_version(&path), 0);

        let conn = open_shared(&path).unwrap();
        assert!(crate::catalog::card_exists(&conn, "sv3pt5-1").unwrap());
        drop(conn);
        assert_eq!(file_user_version(&path), Database::Shared.version() as u32);
    }

    /// Re-opening an up-to-date database leaves its version exactly where it
    /// was — the gate does not creep the number on every start. (That the
    /// stamp writes *nothing at all* in this case is asserted a level down,
    /// in `schema_version`, against the file's change counter.)
    #[test]
    fn re_opening_an_up_to_date_database_writes_no_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        drop(open_user(&path).unwrap());
        let stamped = file_user_version(&path);
        assert_eq!(stamped, Database::User.version() as u32);

        for _ in 0..3 {
            let conn = open_user(&path).unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .unwrap();
            assert_eq!(file_user_version(&path), stamped);
        }
    }

    /// The gate itself: a collection written by a newer build is refused,
    /// and the refusal names both versions and the file. Rollback is only
    /// safe because of this — an older binary must stop, not quietly
    /// operate on a schema it does not know.
    #[test]
    fn a_collection_from_the_future_is_refused_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        seed_shared(&shared_path);
        let path = dir.path().join("collection.sqlite");
        drop(open_user(&path).unwrap());

        let ahead = Database::User.version() + 1;
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(&format!("PRAGMA user_version = {ahead}"))
                .unwrap();
        }

        let err = open_user(&path).unwrap_err();
        assert!(matches!(err, crate::error::DbError::SchemaVersion(_)));
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("version {ahead}")),
            "no file version: {msg}"
        );
        assert!(
            msg.contains(&format!("version {}", Database::User.version())),
            "no binary version: {msg}"
        );
        assert!(msg.contains("collection.sqlite"), "no file named: {msg}");

        // The whole way in, not just the low-level one.
        assert!(connect_user(&path, &shared_path).is_err());
    }

    /// The server reaches the catalog by attaching it, not through
    /// `open_shared` — so the gate has to be on that path as well.
    #[test]
    fn a_catalog_from_the_future_is_refused_on_attach() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        seed_shared(&shared_path);
        let ahead = Database::Shared.version() + 1;
        {
            let c = Connection::open(&shared_path).unwrap();
            c.execute_batch(&format!("PRAGMA user_version = {ahead}"))
                .unwrap();
        }

        let err = connect_user(&dir.path().join("collection.sqlite"), &shared_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("shared.sqlite"), "no file named: {msg}");
        assert!(
            msg.contains(&format!("version {ahead}")),
            "no file version: {msg}"
        );
        assert!(open_shared(&shared_path).is_err(), "and directly, too");
    }

    /// Build a registry the way the binary that predates the gate did:
    /// schema applied straight, no `user_version` written, a real user in it.
    /// Every registry in existence is this shape — the epic that creates the
    /// file and the epic that added the gate were separate branches, so there
    /// has never been a build that stamped one (pd-r60h).
    fn unversioned_registry(path: &Path) -> String {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .unwrap();
        conn.execute_batch(SCHEMA_REGISTRY).unwrap();
        let user = crate::registry::insert(&conn, "alice").unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(conn);
        assert_eq!(
            file_user_version(path),
            0,
            "the fixture must be genuinely unversioned"
        );
        user.database_id
    }

    /// The adoption path for the third database. It is the one that actually
    /// runs: there is no registry anywhere carrying a version, so if this is
    /// wrong, no box with users on it starts.
    #[test]
    fn an_unversioned_registry_is_adopted_in_place_with_its_rows() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.sqlite");
        let database_id = unversioned_registry(&path);
        let before = std::fs::metadata(&path).unwrap().ino();

        let conn = open_registry(&path).unwrap();

        let user = crate::registry::lookup(&conn, "alice").unwrap().unwrap();
        assert_eq!(
            user.database_id, database_id,
            "adoption must not lose the map from handle to database"
        );
        drop(conn);

        assert_eq!(
            file_user_version(&path),
            Database::Registry.version() as u32,
            "the adopted registry must carry the version afterwards"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            before,
            "adoption must happen in place — the file must not be recreated"
        );
    }

    /// A registry from the future is refused rather than written to, and the
    /// refusal names both versions and the file.
    ///
    /// Sharper here than for a collection: this is the file that says whose
    /// database is whose. An older build that applied its own schema over a
    /// newer registry would not damage a collection — it would damage the
    /// only thing that can attribute one.
    #[test]
    fn a_registry_from_the_future_is_refused_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.sqlite");
        drop(open_registry(&path).unwrap());

        let ahead = Database::Registry.version() + 1;
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(&format!(
                "PRAGMA user_version = {ahead}; PRAGMA wal_checkpoint(TRUNCATE);"
            ))
            .unwrap();
        }
        let before = file_change_counter(&path);

        let err = open_registry(&path).unwrap_err();
        assert!(matches!(err, crate::error::DbError::SchemaVersion(_)));
        let msg = err.to_string();
        assert!(
            msg.contains(&format!("version {ahead}")),
            "no file version: {msg}"
        );
        assert!(
            msg.contains(&format!("version {}", Database::Registry.version())),
            "no binary version: {msg}"
        );
        assert!(msg.contains("registry.sqlite"), "no file named: {msg}");
        assert!(
            msg.contains(Database::Registry.label()),
            "no database named: {msg}"
        );

        // Refused means not written to. A refusal that had already applied
        // the schema on the way past would be a refusal of the return value
        // only, which is the failure mode the gate exists to prevent.
        assert_eq!(
            file_change_counter(&path),
            before,
            "the refused registry must not have been touched"
        );
        assert_eq!(file_user_version(&path), ahead as u32);
    }

    /// All THREE databases are gated, not two. The registry was declared in
    /// `schema_version` and wired to nothing for as long as the two epics were
    /// separate branches, and "declared" is not "enforced" (pd-r60h).
    #[test]
    fn every_database_refuses_a_file_from_the_future() {
        let dir = tempfile::tempdir().unwrap();
        let ahead_by_one = |path: &Path, db: Database| {
            let c = Connection::open(path).unwrap();
            c.execute_batch(&format!("PRAGMA user_version = {}", db.version() + 1))
                .unwrap();
        };

        let shared = dir.path().join("shared.sqlite");
        let user = dir.path().join("collection.sqlite");
        let registry = dir.path().join("registry.sqlite");
        drop(open_shared(&shared).unwrap());
        drop(open_user(&user).unwrap());
        drop(open_registry(&registry).unwrap());
        ahead_by_one(&shared, Database::Shared);
        ahead_by_one(&user, Database::User);
        ahead_by_one(&registry, Database::Registry);

        assert!(open_shared(&shared).is_err(), "the catalog is not gated");
        assert!(open_user(&user).is_err(), "the collection is not gated");
        assert!(
            open_registry(&registry).is_err(),
            "the registry is not gated"
        );
    }

    /// The migration-history table the pre-luo migration system left on
    /// every database built before it was removed. Recreated here in the
    /// shape refinery wrote it so the drop is exercised against the real
    /// thing rather than an empty stand-in.
    fn add_legacy_refinery_table(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE refinery_schema_history ( \
                 version INT4 PRIMARY KEY, \
                 name VARCHAR(255), \
                 applied_on VARCHAR(255), \
                 checksum VARCHAR(255)); \
             INSERT INTO refinery_schema_history \
                 VALUES (1, 'initial', '2026-05-18T00:00:00', 'deadbeef');",
        )
        .unwrap();
    }

    fn has_table(conn: &Connection, name: &str) -> bool {
        conn.prepare("SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1")
            .unwrap()
            .exists([name])
            .unwrap()
    }

    /// pd-yj40: the legacy table goes away on open, so no reader downstream
    /// has to know its name. Before the drop the JSON export was the one
    /// keeping it out of a fresh collection — by naming it.
    #[test]
    fn a_legacy_refinery_table_is_dropped_from_a_collection_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        unversioned_collection(&path);
        add_legacy_refinery_table(&path);

        let conn = open_user(&path).unwrap();
        assert!(
            !has_table(&conn, "refinery_schema_history"),
            "the legacy migration table must not survive an open"
        );
        // ...and the collection it sat beside is untouched.
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        // The exporter no longer names it, so this is what keeps it out.
        assert!(
            !crate::json_backup::user_tables(&conn)
                .unwrap()
                .iter()
                .any(|t| t == "refinery_schema_history"),
            "a dropped table cannot reach the JSON envelope"
        );
    }

    /// The catalog carries its own copy. It is dropped by `open_shared` —
    /// `pkdump setup`, the offline derive, the server's own startup — because
    /// those are the paths that hold it read-write. A connection that merely
    /// attaches it, or `open_shared_readonly`, cannot.
    #[test]
    fn a_legacy_refinery_table_is_dropped_from_the_catalog_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.sqlite");
        seed_shared(&path);
        add_legacy_refinery_table(&path);

        let conn = open_shared(&path).unwrap();
        assert!(!has_table(&conn, "refinery_schema_history"));
        assert!(crate::catalog::card_exists(&conn, "sv3pt5-1").unwrap());
    }

    /// The drop is a statement in the schema, which is re-applied on every
    /// single open — so it must cost nothing once there is nothing to drop.
    /// A write here would be a write on every server start, replicated
    /// off-box by Litestream each time (same standard as the version stamp).
    #[test]
    fn dropping_a_table_that_is_already_gone_writes_nothing_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        {
            let conn = open_user(&path).unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .unwrap();
        }
        let before = file_change_counter(&path);

        for _ in 0..3 {
            let conn = open_user(&path).unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
                .unwrap();
            assert_eq!(
                file_change_counter(&path),
                before,
                "re-opening must not write to the database"
            );
        }
    }

    /// A collection written before `conditions` moved into the user schema
    /// (pd-s4c2) — the restored-from-an-old-replica case, and the one
    /// per-file versioning makes genuinely reachable.
    fn pre_move_collection(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .unwrap();
        conn.execute_batch(SCHEMA_USER).unwrap();
        // Put it back the way the previous build left it: no `conditions`,
        // carrying that build's user_version.
        conn.execute_batch("DROP TABLE conditions; PRAGMA user_version = 1;")
            .unwrap();
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source, condition) \
             VALUES ('sv3pt5-1-normal', '2026-08-08', 'manual_id', 'Lightly Played')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(conn);
        assert_eq!(file_user_version(path), 1, "the fixture must be pre-move");
    }

    /// The move IS the migration: an existing collection grows the table and
    /// its five defaults on the next open, keeping its rows, and comes out
    /// stamped with this build's version.
    #[test]
    fn a_collection_written_before_the_move_grows_conditions_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        pre_move_collection(&path);

        let conn = open_user(&path).unwrap();
        assert!(has_table(&conn, "conditions"));
        let m = crate::conditions::multipliers(&conn).unwrap();
        assert_eq!(m.len(), 5);
        assert_eq!(m.get("Lightly Played"), Some(&0.85));
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the migration must not lose the collection");
        drop(conn);

        assert_eq!(file_user_version(&path), Database::User.version() as u32);
    }

    /// The catalog sheds its copy on the one path that holds it read-write.
    #[test]
    fn the_catalogs_conditions_table_is_dropped_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.sqlite");
        seed_shared(&path);
        add_stale_catalog_conditions(&path);

        let conn = open_shared(&path).unwrap();
        assert!(!has_table(&conn, "conditions"));
        assert!(catalog::card_exists(&conn, "sv3pt5-1").unwrap());
    }

    /// A catalog built before the move, still physically carrying the table.
    /// `0.01` so a multiplier read from here is unmistakable.
    fn add_stale_catalog_conditions(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE conditions ( \
                 name TEXT PRIMARY KEY, multiplier REAL NOT NULL, rank INTEGER NOT NULL); \
             INSERT INTO conditions VALUES ('Near Mint', 0.01, 0);",
        )
        .unwrap();
    }

    /// The catalog must never shade the collection's own tables. SQLite
    /// resolves an unqualified name in `temp` before `main`, and
    /// `gate_attached` deliberately accepts a catalog that is *behind* this
    /// build — so a `shared.sqlite` not yet through `pkdump setup` since the
    /// move still holds `conditions`, and a TEMP VIEW over it would silently
    /// become the multipliers every value on the page is computed from.
    #[test]
    fn a_stale_catalog_table_does_not_shade_the_collections_own() {
        let dir = tempfile::tempdir().unwrap();
        let shared_path = dir.path().join("shared.sqlite");
        seed_shared(&shared_path);
        add_stale_catalog_conditions(&shared_path);

        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared_path).unwrap();
        let m = crate::conditions::multipliers(&conn).unwrap();
        assert_eq!(
            m.get("Near Mint"),
            Some(&1.0),
            "the collection's own multiplier must win over the catalog's"
        );
        assert_eq!(
            m.len(),
            5,
            "and the collection's whole seed must be visible"
        );
        // The catalog is still reachable when asked for by name — this is a
        // resolution rule, not a hidden table.
        let stale: f64 = conn
            .query_row("SELECT multiplier FROM shared.conditions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stale, 0.01);
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
