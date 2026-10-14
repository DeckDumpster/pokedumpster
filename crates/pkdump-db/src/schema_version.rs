//! Every database states its own schema version, and the binary refuses to operate on one it does
//! not understand (pd-ja38).
//!
//! The mechanism — `PRAGMA user_version`, the three outcomes (behind is adopted, equal is a
//! no-op, ahead is refused), the reporting read that works on a file the gate would refuse — is
//! the tenancy kernel's (`dumpster_kit_tenancy::schema_version`). What is PokeDumpster's is which
//! databases it has and the version this build understands for each: [`Database`].
//!
//! The refusal is what makes rollback (`pkdump tenant revert`) safe: an older binary stops
//! instead of quietly operating on a schema it does not know.

use std::fmt;

use dumpster_kit_tenancy::SchemaSpec;
use dumpster_kit_tenancy::schema_version as kit;
use rusqlite::Connection;

use crate::error::Result;

pub use dumpster_kit_tenancy::SchemaState;
pub use dumpster_kit_tenancy::schema_version::{version, version_of, version_of_file};

/// One of PokeDumpster's databases, and the schema version this build of the binary understands
/// for it.
///
/// The versions are independent: the files change for unrelated reasons. Bump one when a change
/// to its schema cannot be expressed as `CREATE … IF NOT EXISTS` — i.e. when an older binary
/// reading the new file would be wrong rather than merely missing something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Database {
    /// The immutable card catalog, `shared.sqlite` (`schema_shared.sql`).
    Shared,
    /// A per-tenant collection (`schema_user.sql`).
    User,
    /// The user registry, `registry.sqlite` at the data root. Its version is the kernel's: the
    /// registry schema is the kernel's.
    Registry,
}

/// Shared and User are at 2: pd-s4c2 moved `conditions` out of the catalog and into the
/// collection, which an older binary would get *wrong* rather than merely miss.
const SHARED: SchemaSpec = SchemaSpec {
    label: "shared catalog",
    version: 2,
};
const USER: SchemaSpec = SchemaSpec {
    label: "collection",
    version: 2,
};

impl Database {
    /// The kernel's description of this database: its label and the version this build writes.
    pub const fn spec(self) -> SchemaSpec {
        match self {
            Database::Shared => SHARED,
            Database::User => USER,
            Database::Registry => kit::REGISTRY,
        }
    }

    /// The schema version this build understands.
    pub fn version(self) -> i64 {
        self.spec().version
    }

    /// How this database is named in an operator-facing error.
    pub fn label(self) -> &'static str {
        self.spec().label
    }

    /// Where a file carrying `found` stands relative to this build.
    pub fn state_of(self, found: i64) -> SchemaState {
        self.spec().state_of(found)
    }
}

impl fmt::Display for Database {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The gate, for the connection's own database. Call it *before* applying the schema.
pub fn gate(conn: &Connection, db: Database) -> Result<i64> {
    Ok(kit::gate(conn, db.spec())?)
}

/// The gate, for a database attached under `schema` — only the refusal applies.
pub fn gate_attached(conn: &Connection, db: Database, schema: &str) -> Result<i64> {
    Ok(kit::gate_attached(conn, db.spec(), schema)?)
}

/// Record this build's version. Call it *after* the schema has been applied.
pub fn stamp(conn: &Connection, db: Database) -> Result<()> {
    Ok(kit::stamp(conn, db.spec())?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbError;

    /// The numbers are PokeDumpster's to state — the kernel only enforces them. A bump here is a
    /// decision that an older binary must stop.
    #[test]
    fn the_versions_this_build_understands() {
        assert_eq!(Database::Shared.version(), 2);
        assert_eq!(Database::User.version(), 2);
        assert_eq!(Database::Registry.version(), 1);
    }

    /// A refusal arrives as `DbError::SchemaVersion`, which the server maps to its own status,
    /// naming both versions.
    #[test]
    fn a_database_from_the_future_is_refused_as_a_schema_version_error() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "PRAGMA user_version = {}",
            Database::User.version() + 1
        ))
        .unwrap();
        let err = gate(&conn, Database::User).unwrap_err();
        assert!(matches!(err, DbError::SchemaVersion(_)), "{err:?}");
        let msg = err.to_string();
        assert!(
            msg.contains("version 3") && msg.contains("version 2"),
            "{msg}"
        );
    }

    #[test]
    fn behind_is_adopted_and_stamped() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(gate(&conn, Database::Shared).unwrap(), 0);
        stamp(&conn, Database::Shared).unwrap();
        assert_eq!(version(&conn).unwrap(), Database::Shared.version());
        assert_eq!(
            Database::Shared.state_of(version(&conn).unwrap()),
            SchemaState::Current
        );
    }
}
