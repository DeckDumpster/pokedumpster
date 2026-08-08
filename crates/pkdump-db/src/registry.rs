//! The user registry — the table that joins a *handle* to a *database*.
//!
//! Before this, a tenant's name was three things at once: the value in an
//! unauthenticated header, the filename on disk, and the S3 replica prefix.
//! One string doing all three is what made `pd-pm7b` possible — delete
//! `alice`, recreate `alice`, and the new database lands under the old
//! one's replica stream for the rest of the retention window.
//!
//! Here they are two facts joined by a row:
//!
//! * `handle` — what a request names, what a person types. Renameable.
//! * `database_id` — an opaque ULID, the stem of `tenants/<id>.sqlite`.
//!   Assigned here, never chosen by a caller, never derived from the handle.
//!
//! Recreating a released handle therefore *cannot* inherit the old
//! database or its replica: it gets a fresh ULID, so it is a different file
//! under a different prefix. Not fixed — unreachable.
//!
//! ULID because it is filename-safe and creation-ordered, so a directory
//! listing stays chronologically meaningful once the names stop being
//! human-readable. It is canonically UPPERCASE Crockford base32, which
//! [`crate::paths::validate_tenant_name`] rejects — that validator guards
//! caller-supplied names, and a `database_id` is not one. Turning an id
//! into a path is `pd-rqgv`'s job, not this module's.
//!
//! Resolution, the CLI, and replication all live elsewhere. This module is
//! the schema and its accessor: lookup, insert, rename, detach.

use rusqlite::{Connection, OptionalExtension, params};
use ulid::Ulid;

use crate::error::{DbError, Result};
use crate::paths::{registry_db_path, validate_tenant_name};

/// Separator marking a released handle. `:` is outside the handle charset
/// [`validate_tenant_name`] admits, so a retired handle can never collide
/// with a live one — which is what lets [`detach`] free `alice` while the
/// row that says where alice's bytes are survives.
const RETIRED_MARK: &str = ":detached:";

/// Whether a registered user is live, or has released their handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserState {
    /// A live user: resolvable, and their handle is taken.
    Active,
    /// The handle was released; the database and its replica were kept.
    /// The row survives so those bytes stay attributable to someone.
    Detached,
}

impl UserState {
    /// The value stored in `user.state`.
    pub fn as_str(self) -> &'static str {
        match self {
            UserState::Active => "active",
            UserState::Detached => "detached",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "active" => Ok(UserState::Active),
            "detached" => Ok(UserState::Detached),
            other => Err(DbError::Env(format!(
                "registry: unknown user state {other:?}"
            ))),
        }
    }
}

/// One row of the registry: who they are, and where their collection lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub handle: String,
    pub database_id: String,
    pub created_at: String,
    pub state: UserState,
}

const COLS: &str = "handle, database_id, created_at, state";

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<(String, String, String, String)> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
}

fn into_user(row: (String, String, String, String)) -> Result<User> {
    Ok(User {
        handle: row.0,
        database_id: row.1,
        created_at: row.2,
        state: UserState::parse(&row.3)?,
    })
}

/// Open the registry at the data root, creating it if absent.
pub fn open() -> Result<Connection> {
    crate::connection::open_registry(&registry_db_path()?)
}

/// The user registered under `handle`, if any.
///
/// The handle is a bound parameter and nothing else: it is compared against
/// a column, never concatenated, never turned into a path. An unknown
/// handle — including one full of `../` — is simply not in the table.
pub fn lookup(conn: &Connection, handle: &str) -> Result<Option<User>> {
    let row = conn
        .query_row(
            &format!("SELECT {COLS} FROM user WHERE handle = ?1"),
            params![handle],
            from_row,
        )
        .optional()?;
    row.map(into_user).transpose()
}

/// Register a new user and mint their `database_id`. Returns the new row.
///
/// The id is generated here, never supplied: it is the one guarantee that
/// two users cannot be pointed at one file, and that a recycled handle
/// gets fresh storage.
///
/// Fails with [`DbError::Conflict`] if the handle is taken — by the PRIMARY
/// KEY, not by a check-then-insert, so two concurrent creates cannot both
/// win.
pub fn insert(conn: &Connection, handle: &str) -> Result<User> {
    validate_tenant_name(handle)?;
    let user = User {
        handle: handle.to_string(),
        database_id: Ulid::generate().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        state: UserState::Active,
    };
    conn.execute(
        "INSERT INTO user (handle, database_id, created_at, state) VALUES (?1, ?2, ?3, ?4)",
        params![
            user.handle,
            user.database_id,
            user.created_at,
            user.state.as_str()
        ],
    )
    .map_err(|e| conflict(e, format!("handle {handle:?} is already registered")))?;
    Ok(user)
}

/// Rename `from` to `to`. Returns the renamed row.
///
/// Only the `handle` column is written — the database, its replica prefix
/// and the user's history are all keyed on `database_id` and cannot move
/// because someone changed their name.
pub fn rename(conn: &Connection, from: &str, to: &str) -> Result<User> {
    validate_tenant_name(to)?;
    let user = require(conn, from)?;
    conn.execute(
        "UPDATE user SET handle = ?2 WHERE handle = ?1",
        params![from, to],
    )
    .map_err(|e| conflict(e, format!("handle {to:?} is already registered")))?;
    Ok(User {
        handle: to.to_string(),
        ..user
    })
}

/// Release `handle` and keep the database: the row goes to
/// `state = 'detached'` and its handle is retired to `<handle>:detached:<id>`.
/// Returns the detached row, under its retired handle.
///
/// This is what `tenant remove` becomes. Nothing is deleted — not the file,
/// not the replica — so the retention window stops being the liability
/// `pd-pm7b` made of it and becomes a safety net. Hard deletion is a
/// separate, explicit act.
///
/// The handle is genuinely free afterwards: registering it again mints a
/// new `database_id`, so the new user gets a new file and a new replica
/// prefix. That is the property, and `handle_is_reusable_and_gets_new_storage`
/// is the test that holds it.
pub fn detach(conn: &Connection, handle: &str) -> Result<User> {
    let user = require(conn, handle)?;
    if user.state == UserState::Detached {
        return Err(DbError::Conflict(format!(
            "user {handle:?} is already detached"
        )));
    }
    let retired = format!("{handle}{RETIRED_MARK}{}", user.database_id);
    conn.execute(
        "UPDATE user SET handle = ?2, state = ?3 WHERE handle = ?1",
        params![handle, retired, UserState::Detached.as_str()],
    )?;
    Ok(User {
        handle: retired,
        state: UserState::Detached,
        ..user
    })
}

/// Every registered user, detached ones included, in creation order —
/// which is `database_id` order, ULIDs being time-prefixed.
pub fn list(conn: &Connection) -> Result<Vec<User>> {
    let mut stmt = conn.prepare(&format!("SELECT {COLS} FROM user ORDER BY database_id"))?;
    let rows: Vec<_> = stmt
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    rows.into_iter().map(into_user).collect()
}

/// [`lookup`], but a missing handle is an error rather than `None`.
fn require(conn: &Connection, handle: &str) -> Result<User> {
    lookup(conn, handle)?
        .ok_or_else(|| DbError::NotFound(format!("no user with handle {handle:?}")))
}

/// Report a UNIQUE/PRIMARY KEY violation as a conflict; anything else is
/// the SQLite error it was.
fn conflict(e: rusqlite::Error, msg: String) -> DbError {
    match &e {
        rusqlite::Error::SqliteFailure(f, _)
            if f.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            DbError::Conflict(msg)
        }
        _ => e.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::open_registry;

    fn registry() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_registry(&dir.path().join("registry.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn insert_then_lookup() {
        let (_dir, conn) = registry();
        let created = insert(&conn, "alice").unwrap();
        assert_eq!(created.handle, "alice");
        assert_eq!(created.state, UserState::Active);
        // A canonical ULID: 26 characters of Crockford base32.
        assert_eq!(created.database_id.len(), 26);
        assert!(
            created
                .database_id
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        );

        assert_eq!(lookup(&conn, "alice").unwrap(), Some(created));
        assert_eq!(lookup(&conn, "bob").unwrap(), None);
    }

    #[test]
    fn the_handle_never_names_a_file() {
        // The load-bearing negative: a handle full of traversal is just a
        // string that is not in the table. Nothing constructs a path from
        // it, so there is no path to escape.
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        for hostile in [
            "../../etc/passwd",
            "../alice",
            "alice/../bob",
            "/etc/shadow",
            "alice\0",
        ] {
            assert_eq!(lookup(&conn, hostile).unwrap(), None, "{hostile:?}");
            assert!(insert(&conn, hostile).is_err(), "{hostile:?}");
        }
    }

    #[test]
    fn a_handle_is_unique_by_schema() {
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        let err = insert(&conn, "alice").unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)), "{err:?}");

        // Not a check-then-insert: the PRIMARY KEY refuses it even when the
        // accessor is bypassed entirely.
        let raw = conn.execute(
            "INSERT INTO user (handle, database_id, created_at, state) \
             VALUES ('alice', 'SOMEOTHERID', '2026-08-08T00:00:00Z', 'active')",
            [],
        );
        assert!(raw.is_err(), "the schema must enforce handle uniqueness");
    }

    #[test]
    fn a_database_id_is_unique_by_schema() {
        // Two handles pointing at one file is the failure this forecloses.
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        let raw = conn.execute(
            "INSERT INTO user (handle, database_id, created_at, state) \
             VALUES ('bob', ?1, '2026-08-08T00:00:00Z', 'active')",
            params![alice.database_id],
        );
        assert!(
            raw.is_err(),
            "the schema must enforce database_id uniqueness"
        );
    }

    #[test]
    fn state_is_constrained_by_schema() {
        let (_dir, conn) = registry();
        let raw = conn.execute(
            "INSERT INTO user (handle, database_id, created_at, state) \
             VALUES ('alice', 'SOMEID', '2026-08-08T00:00:00Z', 'banished')",
            [],
        );
        assert!(raw.is_err(), "state must be constrained to the two values");
    }

    #[test]
    fn rename_does_not_touch_the_database_id() {
        let (_dir, conn) = registry();
        let before = insert(&conn, "alice").unwrap();
        let after = rename(&conn, "alice", "alicia").unwrap();

        assert_eq!(after.handle, "alicia");
        assert_eq!(after.database_id, before.database_id);
        assert_eq!(after.created_at, before.created_at);
        assert_eq!(after.state, before.state);

        assert_eq!(lookup(&conn, "alice").unwrap(), None);
        assert_eq!(lookup(&conn, "alicia").unwrap(), Some(after));
    }

    #[test]
    fn rename_rejects_a_taken_or_missing_handle() {
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        insert(&conn, "bob").unwrap();

        let taken = rename(&conn, "alice", "bob").unwrap_err();
        assert!(matches!(taken, DbError::Conflict(_)), "{taken:?}");

        let missing = rename(&conn, "carol", "carla").unwrap_err();
        assert!(matches!(missing, DbError::NotFound(_)), "{missing:?}");

        // A rename that failed changed nothing.
        assert!(lookup(&conn, "alice").unwrap().is_some());
        assert!(lookup(&conn, "bob").unwrap().is_some());

        // The new handle is validated, so a rename cannot smuggle in a
        // name the registry would never have issued.
        assert!(rename(&conn, "alice", "../bob").is_err());
    }

    #[test]
    fn detach_keeps_the_row_and_releases_the_handle() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        let detached = detach(&conn, "alice").unwrap();

        assert_eq!(detached.state, UserState::Detached);
        assert_eq!(detached.database_id, alice.database_id);
        assert_eq!(detached.created_at, alice.created_at);
        // The handle is released...
        assert_eq!(lookup(&conn, "alice").unwrap(), None);
        // ...but the mapping survives, so the file on disk and its replica
        // are still attributable to who owned them.
        let detached_handle = detached.handle.clone();
        assert_eq!(lookup(&conn, &detached_handle).unwrap(), Some(detached));

        // There is no second detach to do: the handle is gone.
        assert!(matches!(
            detach(&conn, "alice").unwrap_err(),
            DbError::NotFound(_)
        ));
        // Nor by naming the retired row directly.
        assert!(matches!(
            detach(&conn, &detached_handle).unwrap_err(),
            DbError::Conflict(_)
        ));
    }

    #[test]
    fn handle_is_reusable_and_gets_new_storage() {
        // pd-pm7b, made unreachable: recreating a released handle cannot
        // inherit the previous holder's database or replica prefix.
        let (_dir, conn) = registry();
        let first = insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();

        let second = insert(&conn, "alice").unwrap();
        assert_ne!(
            second.database_id, first.database_id,
            "a recycled handle must not inherit its predecessor's database"
        );
        assert_eq!(second.state, UserState::Active);
        assert_eq!(lookup(&conn, "alice").unwrap(), Some(second.clone()));

        // Both rows are on the books: one live, one detached.
        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(
            all.iter().filter(|u| u.state == UserState::Active).count(),
            1
        );
        // And the detached row still names the old database, which is what
        // a restore has to be able to find.
        assert!(
            all.iter()
                .any(|u| u.state == UserState::Detached && u.database_id == first.database_id)
        );
    }

    #[test]
    fn detached_handles_cannot_collide_with_live_ones() {
        // Two successive alices, both released: retiring by database_id
        // keeps their rows distinct under a PRIMARY KEY on handle.
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();
        insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();
        assert_eq!(list(&conn).unwrap().len(), 2);

        // A retired handle is not a name anyone could have registered.
        for u in list(&conn).unwrap() {
            assert!(validate_tenant_name(&u.handle).is_err(), "{u:?}");
        }
    }

    #[test]
    fn list_is_in_creation_order() {
        let (_dir, conn) = registry();
        let a = insert(&conn, "alice").unwrap();
        let b = insert(&conn, "bob").unwrap();
        let ids: Vec<String> = list(&conn)
            .unwrap()
            .into_iter()
            .map(|u| u.database_id)
            .collect();
        let mut sorted = vec![a.database_id, b.database_id];
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn the_schema_is_applied_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.sqlite");
        let first = open_registry(&path).unwrap();
        insert(&first, "alice").unwrap();
        drop(first);

        let second = open_registry(&path).unwrap();
        assert!(lookup(&second, "alice").unwrap().is_some());
    }

    #[test]
    fn the_registry_is_created_at_the_data_root() {
        crate::paths::with_home(|home| {
            let conn = open().unwrap();
            insert(&conn, "alice").unwrap();
            assert!(home.join("registry.sqlite").exists());
            // Not a tenant, and not the catalog.
            assert!(!home.join("tenants").join("registry.sqlite").exists());
        });
    }
}
