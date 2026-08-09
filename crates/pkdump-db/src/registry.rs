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
//!
//! What it deliberately is *not* is the place the rules live. `database_id`
//! is the primary key, a handle's charset is a `CHECK`, and "one live user
//! per handle" is a partial unique index over `state = 'active'` — all three
//! in `schema_registry.sql`, so they hold for every writer including an
//! operator with `sqlite3` open on the file. The functions below are the
//! ergonomic path to them, not the enforcement.

use rusqlite::{Connection, OptionalExtension, params};
use ulid::Ulid;

use crate::error::{DbError, Result};
use crate::paths::{registry_db_path, validate_tenant_name};

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
///
/// `database_id` first because it is the primary key — the identity. `handle`
/// is a label on it, and a detached user keeps theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub database_id: String,
    pub handle: String,
    pub created_at: String,
    pub state: UserState,
    /// When the handle was released. `None` while active.
    pub retired_at: Option<String>,
}

const COLS: &str = "database_id, handle, created_at, state, retired_at";

/// The columns as [`COLS`] names them, still as SQLite handed them over.
type Row = (String, String, String, String, Option<String>);

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
}

fn into_user(row: Row) -> Result<User> {
    Ok(User {
        database_id: row.0,
        handle: row.1,
        created_at: row.2,
        state: UserState::parse(&row.3)?,
        retired_at: row.4,
    })
}

/// Open the registry at the data root, creating it if absent.
pub fn open() -> Result<Connection> {
    crate::connection::open_registry(&registry_db_path()?)
}

/// The **active** user registered under `handle`, if any.
///
/// Active-only, and that is the whole meaning of the word "registered" here:
/// a detached row keeps its holder's real handle, so a handle can name one
/// live user and any number of retired ones. Which of them a caller wants is
/// never in question — the live one, or nobody. Retired rows are reached by
/// `database_id` ([`find`]) or read in bulk ([`list`]), the two places that
/// are asking about a *database* rather than about a name.
///
/// At most one row can come back: `user_one_active_handle` is a unique index
/// over exactly this predicate.
///
/// The handle is a bound parameter and nothing else: it is compared against
/// a column, never concatenated, never turned into a path. An unknown
/// handle — including one full of `../` — is simply not in the table.
pub fn lookup(conn: &Connection, handle: &str) -> Result<Option<User>> {
    let row = conn
        .query_row(
            &format!("SELECT {COLS} FROM user WHERE handle = ?1 AND state = 'active'"),
            params![handle],
            from_row,
        )
        .optional()?;
    row.map(into_user).transpose()
}

/// Mint a fresh `database_id`. The one place ids come from.
///
/// A canonical ULID: 26 characters of uppercase Crockford base32, which is
/// exactly what [`crate::paths::validate_database_id`] admits — that
/// function is the gate every id passes on its way to becoming a path, and
/// this is the only thing on the far side of it.
pub fn mint_database_id() -> String {
    Ulid::generate().to_string()
}

/// Register a new user and mint their `database_id`. Returns the new row.
///
/// The id is generated here, never supplied: it is the one guarantee that
/// two users cannot be pointed at one file, and that a recycled handle
/// gets fresh storage.
///
/// Fails with [`DbError::Conflict`] if the handle is taken by a live user —
/// by `user_one_active_handle`, not by a check-then-insert, so two concurrent
/// creates cannot both win. A handle held only by *detached* rows is free,
/// which is the point of the index being partial.
///
/// [`validate_tenant_name`] runs first for the error message, not for the
/// guarantee: the `CHECK` on `user.handle` is the same rule and it is the one
/// that cannot be gone around.
pub fn insert(conn: &Connection, handle: &str) -> Result<User> {
    validate_tenant_name(handle)?;
    let user = User {
        database_id: mint_database_id(),
        handle: handle.to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        state: UserState::Active,
        retired_at: None,
    };
    conn.execute(
        &format!("INSERT INTO user ({COLS}) VALUES (?1, ?2, ?3, ?4, ?5)"),
        params![
            user.database_id,
            user.handle,
            user.created_at,
            user.state.as_str(),
            user.retired_at
        ],
    )
    .map_err(|e| conflict(e, format!("handle {handle:?} is already registered")))?;
    Ok(user)
}

/// Rename `from` to `to`. Returns the renamed row.
///
/// Only the `handle` column is written, and the row is addressed by its
/// `database_id` — the database, its replica prefix and the user's history
/// are all keyed on that id and cannot move because someone changed their
/// name.
pub fn rename(conn: &Connection, from: &str, to: &str) -> Result<User> {
    validate_tenant_name(to)?;
    let user = require(conn, from)?;
    conn.execute(
        "UPDATE user SET handle = ?2 WHERE database_id = ?1",
        params![user.database_id, to],
    )
    .map_err(|e| conflict(e, format!("handle {to:?} is already registered")))?;
    Ok(User {
        handle: to.to_string(),
        ..user
    })
}

/// Release `handle` and keep the database: the row goes to
/// `state = 'detached'`, stamped with when. Returns the detached row.
///
/// This is what `tenant remove` becomes. Nothing is deleted — not the file,
/// not the replica — so the retention window stops being the liability
/// `pd-pm7b` made of it and becomes a safety net. Hard deletion is a
/// separate, explicit act.
///
/// **The retired row keeps the person's real handle.** Freeing the name is
/// the index's job, not a rewrite's: `user_one_active_handle` covers only
/// `state = 'active'`, so the moment this `UPDATE` commits the handle is
/// available and the row still says whose bytes those are. An orphaned
/// database is therefore attributable by reading a column rather than by
/// parsing a composite string.
///
/// The handle is genuinely free afterwards: registering it again mints a
/// new `database_id`, so the new user gets a new file and a new replica
/// prefix. That is the property, and `handle_is_reusable_and_gets_new_storage`
/// is the test that holds it.
pub fn detach(conn: &Connection, handle: &str) -> Result<User> {
    let user = require(conn, handle)?;
    let retired_at = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE user SET state = ?2, retired_at = ?3 WHERE database_id = ?1",
        params![user.database_id, UserState::Detached.as_str(), retired_at],
    )?;
    Ok(User {
        state: UserState::Detached,
        retired_at: Some(retired_at),
        ..user
    })
}

/// The user whose collection lives in `database_id`, if any.
///
/// The inverse of the map: a file on disk back to whoever it belongs to.
/// What a purge and a post-restore audit both start from.
pub fn find(conn: &Connection, database_id: &str) -> Result<Option<User>> {
    let row = conn
        .query_row(
            &format!("SELECT {COLS} FROM user WHERE database_id = ?1"),
            params![database_id],
            from_row,
        )
        .optional()?;
    row.map(into_user).transpose()
}

/// Forget a detached user entirely — the registry half of a hard delete.
/// Returns the row that was removed.
///
/// Refuses an `active` user, which is not a policy but the invariant: an
/// active row is what makes a database reachable, and dropping it would
/// leave bytes on disk that belong to nobody. [`detach`] first, deliberately,
/// then this. Deleting the file is [`crate::tenants::purge`]'s half.
pub fn delete(conn: &Connection, database_id: &str) -> Result<User> {
    let user = find(conn, database_id)?
        .ok_or_else(|| DbError::NotFound(format!("no user with database id {database_id:?}")))?;
    if user.state == UserState::Active {
        return Err(DbError::Conflict(format!(
            "user {:?} is still active — detach them before forgetting the mapping",
            user.handle
        )));
    }
    conn.execute(
        "DELETE FROM user WHERE database_id = ?1",
        params![database_id],
    )?;
    Ok(user)
}

/// Drop an **active** user's row without touching their database — the
/// registry half of `pd-hqee`'s rollback, and the only thing that may do it.
///
/// [`delete`] refuses an active user because dropping their row would leave
/// bytes on disk that belong to nobody. That invariant is about
/// *attributability*, not about the row: [`crate::tenants::unmigrate`] renames
/// `tenants/<database_id>.sqlite` back to `tenants/<handle>.sqlite` **first**,
/// so by the time this runs the file is attributable by its own name again —
/// which is what the pre-registry layout meant by a tenant existing. Calling
/// it in the other order would produce exactly the anonymous database
/// [`delete`] exists to prevent.
///
/// It is not a detach: a rollback has to leave the handle free and the
/// registry with nothing to say about it, because the build being rolled back
/// to does not read the registry at all.
pub fn unregister(conn: &Connection, database_id: &str) -> Result<User> {
    let user = find(conn, database_id)?
        .ok_or_else(|| DbError::NotFound(format!("no user with database id {database_id:?}")))?;
    conn.execute(
        "DELETE FROM user WHERE database_id = ?1",
        params![database_id],
    )?;
    Ok(user)
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
///
/// "Missing" includes a handle only detached rows hold: [`lookup`] is
/// active-only, so [`rename`] and [`detach`] can act on what it returns
/// without asking a second time whether the user is live.
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

    /// Insert a row past the accessor entirely, so the assertion is about the
    /// schema and not about Rust. Returns whatever SQLite said.
    fn raw_insert(
        conn: &Connection,
        database_id: &str,
        handle: &str,
        state: &str,
    ) -> rusqlite::Result<usize> {
        conn.execute(
            "INSERT INTO user (database_id, handle, created_at, state) \
             VALUES (?1, ?2, '2026-08-08T00:00:00Z', ?3)",
            params![database_id, handle, state],
        )
    }

    #[test]
    fn one_active_user_per_handle_by_schema() {
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        let err = insert(&conn, "alice").unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)), "{err:?}");

        // Not a check-then-insert, and not the accessor being careful: the
        // partial unique index refuses a second ACTIVE alice even when the
        // accessor is bypassed entirely.
        assert!(
            raw_insert(&conn, "SOMEOTHERID", "alice", "active").is_err(),
            "the schema must refuse two active users with one handle"
        );

        // ...and admits a DETACHED one, which is the whole reason the index
        // is partial: a released handle is free while its row survives.
        raw_insert(&conn, "SOMEOTHERID", "alice", "detached")
            .expect("a detached row may share a handle with a live user");
    }

    #[test]
    fn a_database_id_is_the_primary_key() {
        // Two handles pointing at one file is the failure this forecloses,
        // and the id being the key is the epic's thesis in the schema.
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        assert!(
            raw_insert(&conn, &alice.database_id, "bob", "active").is_err(),
            "the schema must enforce database_id uniqueness"
        );
    }

    #[test]
    fn state_is_constrained_by_schema() {
        let (_dir, conn) = registry();
        assert!(
            raw_insert(&conn, "SOMEID", "alice", "banished").is_err(),
            "state must be constrained to the two values"
        );
    }

    /// The SQL half of [`crate::paths::HANDLE_CASES`]; the Rust half is
    /// `paths::tests::tenant_names_are_validated`, over the same list.
    ///
    /// Two things at once. First, the definition of a valid handle is part of
    /// the data model, so it holds against a writer that never goes near
    /// `validate_tenant_name` — a migration, or an operator with sqlite3 open
    /// on the file. Second, and the reason the corpus is shared: the `CHECK`
    /// and the validator must not drift. The validator is what a request is
    /// refused by at the boundary (`pd-4g7c`), and the `CHECK` is what a row is
    /// refused by; a handle either of them admits and the other does not is a
    /// request answered wrongly — 400 for a name that could have been
    /// registered, or 404 for one that could not.
    #[test]
    fn the_check_and_the_validator_agree() {
        let (_dir, conn) = registry();

        // A distinct id each time, so the CHECK is the only thing that can
        // refuse the row — a shared id would fail on the primary key and read
        // as a pass whatever the constraint did.
        for (i, (handle, valid)) in crate::paths::HANDLE_CASES.iter().enumerate() {
            let written = raw_insert(&conn, &format!("ID{i:024}"), handle, "active");
            assert_eq!(
                written.is_ok(),
                *valid,
                "the CHECK and validate_tenant_name disagree about {handle:?}: {written:?}"
            );
            if let Err(e) = written {
                assert!(
                    e.to_string().contains("CHECK constraint failed"),
                    "{handle:?} must be refused by the CHECK, not by something else: {e}"
                );
            }
        }
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
        assert_eq!(alice.retired_at, None);
        let detached = detach(&conn, "alice").unwrap();

        assert_eq!(detached.state, UserState::Detached);
        assert_eq!(detached.database_id, alice.database_id);
        assert_eq!(detached.created_at, alice.created_at);
        // The row is stamped with when the handle was released.
        assert!(detached.retired_at.is_some(), "{detached:?}");
        // The handle is released — nothing live answers to it...
        assert_eq!(lookup(&conn, "alice").unwrap(), None);
        // ...and yet the row still carries alice's REAL handle, so the file
        // on disk and its replica stay attributable to who owned them
        // without anything having to parse a composite string.
        assert_eq!(detached.handle, "alice");
        assert_eq!(find(&conn, &alice.database_id).unwrap(), Some(detached));

        // There is no second detach to do: no live user answers to the name.
        assert!(matches!(
            detach(&conn, "alice").unwrap_err(),
            DbError::NotFound(_)
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
        // And the detached row still names the old database *and* the handle
        // it was held under, which is what a restore has to be able to find.
        assert!(all.iter().any(|u| u.state == UserState::Detached
            && u.database_id == first.database_id
            && u.handle == "alice"));
    }

    #[test]
    fn a_handle_may_be_retired_any_number_of_times() {
        // Two alices released and a third live: three rows all named
        // "alice", exactly one of them active. Under a PRIMARY KEY on
        // handle this shape was unrepresentable, which is why detach used
        // to have to rewrite the name.
        let (_dir, conn) = registry();
        insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();
        insert(&conn, "alice").unwrap();
        detach(&conn, "alice").unwrap();
        let live = insert(&conn, "alice").unwrap();

        let all = list(&conn).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(|u| u.handle == "alice"), "{all:?}");
        assert_eq!(
            all.iter()
                .filter(|u| u.state == UserState::Active)
                .collect::<Vec<_>>(),
            vec![&live]
        );
        // Every retired row keeps the handle its holder actually had — the
        // property an orphaned database is identified by.
        for u in all.iter().filter(|u| u.state == UserState::Detached) {
            validate_tenant_name(&u.handle).unwrap();
            assert!(u.retired_at.is_some(), "{u:?}");
        }
        // And each is a different database. Three alices, three files.
        let ids: std::collections::HashSet<&str> =
            all.iter().map(|u| u.database_id.as_str()).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn find_maps_a_database_back_to_its_owner() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        assert_eq!(find(&conn, &alice.database_id).unwrap(), Some(alice));
        assert_eq!(find(&conn, "NOSUCHDATABASE").unwrap(), None);
    }

    #[test]
    fn delete_refuses_an_active_user() {
        // Dropping the row of a live user would leave their bytes on disk
        // attributable to nobody. Detaching is the deliberate first step.
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        let err = delete(&conn, &alice.database_id).unwrap_err();
        assert!(matches!(err, DbError::Conflict(_)), "{err:?}");
        assert_eq!(lookup(&conn, "alice").unwrap(), Some(alice.clone()));

        detach(&conn, "alice").unwrap();
        let gone = delete(&conn, &alice.database_id).unwrap();
        assert_eq!(gone.database_id, alice.database_id);
        assert_eq!(gone.state, UserState::Detached);
        assert_eq!(find(&conn, &alice.database_id).unwrap(), None);
        assert_eq!(list(&conn).unwrap(), Vec::new());

        // And there is nothing left to delete twice.
        assert!(matches!(
            delete(&conn, &alice.database_id).unwrap_err(),
            DbError::NotFound(_)
        ));
    }

    /// The rollback's registry half: an ACTIVE row goes, the handle comes
    /// free, and nothing is left saying the user was ever registered — which
    /// is the state a build predating the registry expects to find.
    #[test]
    fn unregister_drops_an_active_row() {
        let (_dir, conn) = registry();
        let alice = insert(&conn, "alice").unwrap();
        // `delete` will not do this, and that is the distinction: it guards
        // attributability, and only the caller that renames the file back to
        // the handle first is entitled to bypass it.
        assert!(delete(&conn, &alice.database_id).is_err());

        let gone = unregister(&conn, &alice.database_id).unwrap();
        assert_eq!(gone, alice);
        assert_eq!(lookup(&conn, "alice").unwrap(), None);
        assert_eq!(find(&conn, &alice.database_id).unwrap(), None);
        assert_eq!(list(&conn).unwrap(), Vec::new());

        // The handle is genuinely free, not retired.
        assert!(insert(&conn, "alice").is_ok());
        // And there is nothing left to drop twice.
        assert!(matches!(
            unregister(&conn, &alice.database_id).unwrap_err(),
            DbError::NotFound(_)
        ));
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
