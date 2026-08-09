//! Every database states its own schema version, and the binary refuses to
//! operate on one it does not understand (pd-ja38).
//!
//! The mechanism is SQLite's own `PRAGMA user_version`: one integer in the
//! file header, no dependency, no history table, and — the property that
//! actually matters here — **per file**. There is one collection database
//! per tenant now, so "which of my N databases are behind" has to have an
//! answer that is read off each file rather than assumed.
//!
//! This is not a migration runner and must not become one. Additive schema
//! change still travels by re-applying `CREATE … IF NOT EXISTS` on every
//! open, exactly as before. `user_version` exists to gate what idempotent
//! application *cannot* express — anything that transforms or drops.
//!
//! ## The three outcomes
//!
//! On open, the file's version is compared with the version this build
//! understands ([`Database::version`]):
//!
//! | file vs binary | outcome |
//! |----------------|---------|
//! | equal          | proceed; [`stamp`] writes nothing |
//! | lower (incl. 0)| apply the schema idempotently, then [`stamp`] |
//! | higher         | **refuse to open** — [`DbError::SchemaVersion`] |
//!
//! Every database in existence as this lands is version 0, prod's included,
//! so the *lower* row is the adoption path and it has to be seamless. It is
//! also the only path a brand-new file takes, which is why adoption and
//! creation are not distinguished here: a fresh database and a
//! pre-`user_version` one are the same case, and giving them one code path
//! is what keeps the rarely-exercised one honest.
//!
//! ## Why the refusal is the point
//!
//! Rollback is supported (`pkdump tenant revert` exists precisely so a build
//! predating the tenant layout finds its collection where it left it), and
//! rollback is only *safe* if the older binary stops rather than quietly
//! operating on a schema it does not know. Without the gate a rollback
//! silently corrupts; with it, the operator gets both version numbers and
//! the path of the file that is too new.

use std::fmt;

use rusqlite::Connection;

use crate::error::{DbError, Result};

/// One of PokeDumpster's databases, and the schema version this build of the
/// binary understands for it.
///
/// The versions are independent: the three files change for unrelated
/// reasons and there is no ordering between them. Bump one when a change to
/// its schema cannot be expressed as `CREATE … IF NOT EXISTS` — i.e. when an
/// older binary reading the new file would be wrong rather than merely
/// missing something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Database {
    /// The immutable card catalog, `shared.sqlite`
    /// (`crates/pkdump-db/src/schema_shared.sql`).
    Shared,
    /// A per-tenant collection, `tenants/<tenant>.sqlite`
    /// (`crates/pkdump-db/src/schema_user.sql`).
    User,
    /// The tenant registry.
    ///
    /// Declared here — with its version and its label — because the versions
    /// of all three databases belong in one place, but not yet wired to an
    /// open path: the registry database itself is being built on a sibling
    /// epic (`integration/logical-vs-physical-…`) and does not exist in this
    /// tree. When it lands, its open function gates and stamps with
    /// `Database::Registry` exactly as [`crate::open_shared`] does with
    /// `Database::Shared` — no new mechanism.
    Registry,
}

impl Database {
    /// The schema version this build understands.
    ///
    /// All three start at 1. Every database on disk today is 0 — the
    /// pre-`user_version` state — so every one of them is adopted on its
    /// next open.
    pub fn version(self) -> i64 {
        match self {
            Database::Shared => 1,
            Database::User => 1,
            Database::Registry => 1,
        }
    }

    /// How this database is named in an operator-facing error.
    pub fn label(self) -> &'static str {
        match self {
            Database::Shared => "shared catalog",
            Database::User => "collection",
            Database::Registry => "tenant registry",
        }
    }
}

impl fmt::Display for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The version recorded in an attached schema's file header (`main` for the
/// connection's own database).
pub fn version_of(conn: &Connection, schema: &str) -> Result<i64> {
    // `PRAGMA` takes no bind parameters, so the schema name is formatted in.
    // It is never caller-supplied — only the two literals below.
    Ok(conn.query_row(&format!("PRAGMA {schema}.user_version"), [], |r| r.get(0))?)
}

/// The version recorded in the connection's own database.
pub fn version(conn: &Connection) -> Result<i64> {
    version_of(conn, "main")
}

/// The gate, for the connection's own database. Call it *before* applying
/// the schema: a file from the future must not be written to at all.
///
/// Returns the version found, so a caller can tell adoption (lower) from a
/// no-op (equal) if it cares. Both proceed.
pub fn gate(conn: &Connection, db: Database) -> Result<i64> {
    gate_schema(conn, db, "main")
}

/// The gate, for a database attached under `schema` — the read-only catalog
/// on a collection connection.
///
/// Only the refusal applies here. An attached catalog is opened read-only
/// and its schema is not applied through this connection, so a *lower*
/// version is not this caller's to adopt: it means the catalog was built by
/// an older binary and `pkdump setup` / `pkdump data refresh` will stamp it
/// on their next run. Refusing it instead would take prod down on a version
/// skew that is harmless in this direction.
pub fn gate_attached(conn: &Connection, db: Database, schema: &str) -> Result<i64> {
    gate_schema(conn, db, schema)
}

fn gate_schema(conn: &Connection, db: Database, schema: &str) -> Result<i64> {
    let found = version_of(conn, schema)?;
    let known = db.version();
    if found > known {
        return Err(DbError::SchemaVersion(format!(
            "the {} database at {} is schema version {found}, but this build of pkdump \
             understands version {known} — refusing to open it. It was written by a newer \
             build; run that build, or restore this database from a replica taken before \
             the upgrade.",
            db.label(),
            file_of(conn, schema),
        )));
    }
    Ok(found)
}

/// Record this build's version on the connection's own database. Call it
/// *after* the schema has been applied — a file stamped before the CREATEs
/// ran would claim a shape it does not have if the process died between.
///
/// A no-op when the file already carries the version: re-opening an
/// up-to-date database writes nothing at all.
pub fn stamp(conn: &Connection, db: Database) -> Result<()> {
    let known = db.version();
    if version(conn)? != known {
        // Formatted, not bound, for the same reason as in `version_of`.
        conn.execute_batch(&format!("PRAGMA user_version = {known}"))?;
    }
    Ok(())
}

/// The file backing `schema`, for the refusal message. Best-effort: an error
/// path is no place to fail, so a database that cannot be located degrades
/// to its schema name rather than swallowing the version mismatch.
fn file_of(conn: &Connection, schema: &str) -> String {
    conn.query_row(
        "SELECT file FROM pragma_database_list WHERE name = ?1",
        [schema],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .filter(|f| !f.is_empty())
    .unwrap_or_else(|| format!("<{schema}>"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn unversioned() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(version(&conn).unwrap(), 0, "a fresh file starts at 0");
        conn
    }

    #[test]
    fn stamp_records_the_builds_version_and_re_stamping_is_a_no_op() {
        let conn = unversioned();
        stamp(&conn, Database::User).unwrap();
        assert_eq!(version(&conn).unwrap(), Database::User.version());
        stamp(&conn, Database::User).unwrap();
        assert_eq!(version(&conn).unwrap(), Database::User.version());
    }

    /// "Re-opening an up-to-date database is a no-op" as a property of the
    /// bytes, not of the value read back: SQLite bumps the *file change
    /// counter* (header bytes 24..28) on every write to the database, so an
    /// unchanged counter is proof that stamping an already-stamped file
    /// touched nothing. A redundant write here would be a write on every
    /// single server start, replicated off-box by Litestream each time.
    #[test]
    fn stamping_an_already_stamped_file_writes_nothing_at_all() {
        fn change_counter(path: &Path) -> u32 {
            let bytes = std::fs::read(path).unwrap();
            u32::from_be_bytes(bytes[24..28].try_into().unwrap())
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE t (x)").unwrap();
            stamp(&conn, Database::User).unwrap();
        }
        let before = change_counter(&path);

        {
            let conn = Connection::open(&path).unwrap();
            stamp(&conn, Database::User).unwrap();
        }
        assert_eq!(change_counter(&path), before);
    }

    #[test]
    fn an_unversioned_database_is_adopted_not_refused() {
        let conn = unversioned();
        assert_eq!(gate(&conn, Database::Shared).unwrap(), 0);
    }

    #[test]
    fn a_database_from_the_future_is_refused_naming_both_versions() {
        let conn = unversioned();
        let ahead = Database::User.version() + 1;
        conn.execute_batch(&format!("PRAGMA user_version = {ahead}"))
            .unwrap();

        let err = gate(&conn, Database::User).unwrap_err();
        assert!(
            matches!(err, DbError::SchemaVersion(_)),
            "wrong kind: {err}"
        );
        let msg = err.to_string();
        assert!(msg.contains(Database::User.label()), "no database: {msg}");
        assert!(msg.contains(&ahead.to_string()), "no file version: {msg}");
        assert!(
            msg.contains(&Database::User.version().to_string()),
            "no binary version: {msg}"
        );
    }

    /// Equal is not "higher" — the version this build writes is the version
    /// it accepts. An off-by-one here would reject every database the binary
    /// itself just stamped.
    #[test]
    fn the_version_this_build_writes_is_one_it_accepts() {
        let conn = unversioned();
        stamp(&conn, Database::Shared).unwrap();
        assert_eq!(
            gate(&conn, Database::Shared).unwrap(),
            Database::Shared.version()
        );
    }

    /// The gate reads the *attached* file's header, not `main`'s. Pointing
    /// it at the wrong schema would make the catalog check silently test the
    /// collection instead.
    #[test]
    fn the_attached_gate_reads_the_attached_file() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = dir.path().join("shared.sqlite");
        {
            let c = Connection::open(&catalog).unwrap();
            let ahead = Database::Shared.version() + 1;
            c.execute_batch(&format!(
                "CREATE TABLE t (x); PRAGMA user_version = {ahead};"
            ))
            .unwrap();
        }

        let conn = Connection::open_in_memory().unwrap();
        conn.execute("ATTACH DATABASE ?1 AS shared", [catalog.to_str().unwrap()])
            .unwrap();

        // `main` is fine; the attached catalog is from the future.
        gate(&conn, Database::User).unwrap();
        let err = gate_attached(&conn, Database::Shared, "shared").unwrap_err();
        assert!(
            err.to_string().contains("shared.sqlite"),
            "the refusal must name the file: {err}"
        );
    }

    /// A catalog behind the binary is not the collection connection's to
    /// adopt — `pkdump setup` stamps it. Refusing it would take prod down on
    /// a skew that is harmless in this direction.
    #[test]
    fn the_attached_gate_accepts_a_catalog_that_is_behind() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = dir.path().join("shared.sqlite");
        Connection::open(&catalog)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();

        let conn = Connection::open_in_memory().unwrap();
        conn.execute("ATTACH DATABASE ?1 AS shared", [catalog.to_str().unwrap()])
            .unwrap();
        assert_eq!(gate_attached(&conn, Database::Shared, "shared").unwrap(), 0);
    }
}
