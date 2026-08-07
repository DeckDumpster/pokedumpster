//! Tenant provisioning — creating, listing and removing the per-tenant
//! collection databases described in [`crate::paths`].
//!
//! A tenant *is* its database file. There is no tenant registry table and
//! deliberately so: a registry would be a second source of truth that can
//! disagree with the filesystem, and the filesystem is what Litestream
//! replicates. `tenants/<name>.sqlite` exists ⇒ the tenant exists.
//!
//! The shared catalog is untouched by every function here. Provisioning a
//! tenant creates one file holding the user schema; the catalog stays a
//! single copy at the root of the data dir, `ATTACH`ed read-only per
//! connection exactly as it always was.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::Connection;

use crate::connection::open_user;
use crate::error::{DbError, Result};
use crate::paths::{legacy_user_db_path, tenant_db_path, tenants_dir, validate_tenant_name};

/// Sidecar files SQLite keeps beside a database in WAL mode.
const WAL_SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// Create tenant `name`: its collection database, with the user schema
/// applied, under `tenants/`. Returns the path.
///
/// Fails if the tenant already exists — provisioning is not idempotent on
/// purpose. "Create the tenant that is already there" is either a typo or a
/// second operator, and silently succeeding would make the second case look
/// like the first.
pub fn create(name: &str) -> Result<PathBuf> {
    let path = tenant_db_path(name)?;
    if path.exists() {
        return Err(DbError::Conflict(format!(
            "tenant {name:?} already exists at {}",
            path.display()
        )));
    }
    // `open_user` creates the parent directory and applies the user schema,
    // so a freshly created tenant is immediately usable.
    open_user(&path)?;
    Ok(path)
}

/// Remove tenant `name`: its collection database, its WAL sidecars, and its
/// Litestream bookkeeping directory. Returns the path that was removed.
///
/// This destroys the only copy of that tenant's collection on this box. The
/// replica in S3 outlives it (see `deploy/RESTORE.md`), but retention is
/// finite — treat this as permanent.
///
/// The Litestream directory goes with it deliberately: leaving it behind
/// would hand a later tenant of the same name a predecessor's replication
/// state, which is the shape of the cross-tenant substitution bug the
/// backup spike found (`deep-dives/litestream-multi-db/RESULT.md` §2).
pub fn remove(name: &str) -> Result<PathBuf> {
    let path = tenant_db_path(name)?;
    if !path.exists() {
        return Err(DbError::NotFound(format!(
            "tenant {name:?} — no database at {}",
            path.display()
        )));
    }
    std::fs::remove_file(&path).map_err(|e| {
        DbError::Env(format!(
            "removing tenant {name:?} ({}): {e}",
            path.display()
        ))
    })?;
    remove_sidecars(&path);
    let _ = std::fs::remove_dir_all(litestream_dir(&path));
    Ok(path)
}

/// Every tenant on this box, sorted. Reads the directory — the filesystem
/// is the registry.
///
/// Files that are not `<valid-tenant-name>.sqlite` are skipped rather than
/// reported: `-wal`/`-shm` sidecars and `.bak` snapshots live in the same
/// directory and are not tenants.
pub fn list() -> Result<Vec<String>> {
    let dir = tenants_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| DbError::Env(format!("reading {}: {e}", dir.display())))?;
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_suffix(".sqlite").map(str::to_string))
        .filter(|n| validate_tenant_name(n).is_ok())
        .collect();
    names.sort();
    Ok(names)
}

/// Whether tenant `name` has a database on this box.
pub fn exists(name: &str) -> Result<bool> {
    Ok(tenant_db_path(name)?.exists())
}

/// Move a pre-`tenants/` collection database into the tenant layout:
/// `$PKDUMP_HOME/<name>.sqlite` → `$PKDUMP_HOME/tenants/<name>.sqlite`.
///
/// This is the migration for the existing production database — it becomes
/// tenant `collection`, the first tenant. [`revert`] is its exact inverse,
/// and both are `rename(2)` within one directory tree, so neither copies
/// bytes and neither can half-finish.
///
/// The database must not be open elsewhere: the WAL is checkpointed and
/// truncated first so the moved file is complete on its own, and a
/// checkpoint that cannot complete because another connection holds the
/// database is reported rather than worked around. Stop the service first.
pub fn adopt(name: &str) -> Result<PathBuf> {
    let from = legacy_user_db_path(name)?;
    let to = tenant_db_path(name)?;
    relocate(name, &from, &to)
}

/// The rollback for [`adopt`]: move a tenant's database back to the
/// pre-`tenants/` location, where a build without the tenant layout will
/// find it. Same mechanics, opposite direction.
pub fn revert(name: &str) -> Result<PathBuf> {
    let from = tenant_db_path(name)?;
    let to = legacy_user_db_path(name)?;
    relocate(name, &from, &to)
}

/// Checkpoint `from`, then `rename(2)` it to `to`. Shared by [`adopt`] and
/// [`revert`] so the rollback cannot drift from the migration.
fn relocate(name: &str, from: &Path, to: &Path) -> Result<PathBuf> {
    if !from.exists() {
        return Err(DbError::NotFound(format!(
            "no collection database for tenant {name:?} at {}",
            from.display()
        )));
    }
    let (from_ls, to_ls) = (litestream_dir(from), litestream_dir(to));
    for dest in [to, &to_ls] {
        if dest.exists() {
            return Err(DbError::Conflict(format!(
                "refusing to move {} onto the existing {}",
                from.display(),
                dest.display()
            )));
        }
    }
    checkpoint_and_close(from)?;
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| DbError::Env(format!("creating {}: {e}", parent.display())))?;
    }
    std::fs::rename(from, to).map_err(|e| {
        DbError::Env(format!(
            "moving {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })?;

    // Litestream 0.5 keeps its replication bookkeeping (LTX cache, txid) in a
    // `.<db>-litestream` directory beside the database — prod has one. Leaving
    // it behind makes the moved database look brand new to the sidecar while
    // its S3 prefix already holds history, so it travels with the file.
    if from_ls.exists()
        && let Err(e) = std::fs::rename(&from_ls, &to_ls)
    {
        // Put the database back so the operator is left with the layout they
        // started from rather than a half-moved one.
        let _ = std::fs::rename(to, from);
        return Err(DbError::Env(format!(
            "moved {} but could not move its Litestream directory {} to {}: {e} \
             (the database was moved back)",
            from.display(),
            from_ls.display(),
            to_ls.display()
        )));
    }

    // The checkpoint emptied these; they describe a file that is no longer
    // there, and a stale `-wal` beside a database is a loaded gun.
    remove_sidecars(from);
    Ok(to.to_path_buf())
}

/// Fold the WAL back into the main database file and truncate it, so the
/// single file that gets renamed holds every committed transaction.
///
/// `wal_checkpoint(TRUNCATE)` reports `busy = 1` instead of failing when
/// another connection is using the database. That is precisely the case
/// that must not proceed — moving a file out from under a live server
/// leaves it writing to an unlinked inode — so it is turned into an error.
fn checkpoint_and_close(path: &Path) -> Result<()> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
    if busy != 0 {
        return Err(DbError::Conflict(format!(
            "{} is open in another process — stop the app (and the Litestream \
             sidecar) before migrating",
            path.display()
        )));
    }
    drop(conn);

    // Belt and braces: TRUNCATE leaves a zero-length WAL. Anything else
    // means frames survived the checkpoint and the move would lose them.
    let wal = sidecar(path, "-wal");
    if let Ok(meta) = std::fs::metadata(&wal)
        && meta.len() > 0
    {
        return Err(DbError::Conflict(format!(
            "{} still holds {} bytes after a TRUNCATE checkpoint — refusing to \
             move an incomplete database",
            wal.display(),
            meta.len()
        )));
    }
    Ok(())
}

/// `…/x.sqlite` → `…/.x.sqlite-litestream`, the directory Litestream 0.5
/// keeps its per-database replication state in.
///
/// Litestream owns this directory; PokeDumpster only ever moves or deletes
/// it as a unit, alongside the database it belongs to.
fn litestream_dir(path: &Path) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let parent = path.parent().unwrap_or(Path::new("."));
    parent.join(format!(".{name}-litestream"))
}

/// `<db>` → `<db>-wal` / `<db>-shm`.
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(suffix);
    PathBuf::from(p)
}

/// Best-effort removal of a database's WAL sidecars.
fn remove_sidecars(path: &Path) {
    for suffix in WAL_SIDECARS {
        let _ = std::fs::remove_file(sidecar(path, suffix));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;
    use crate::connection::{connect_user, open_shared};
    use crate::paths::{shared_db_path, with_home};

    fn seed_catalog() {
        let conn = open_shared(&shared_db_path().unwrap()).unwrap();
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
    fn create_list_remove_round_trip() {
        with_home(|home| {
            assert_eq!(list().unwrap(), Vec::<String>::new());

            let path = create("alice").unwrap();
            assert_eq!(path, home.join("tenants").join("alice.sqlite"));
            create("bob").unwrap();
            assert_eq!(list().unwrap(), vec!["alice", "bob"]);
            assert!(exists("alice").unwrap());

            // Creating an existing tenant is an error, not a no-op.
            assert!(create("alice").is_err());

            remove("alice").unwrap();
            assert_eq!(list().unwrap(), vec!["bob"]);
            assert!(!exists("alice").unwrap());
            // Removing a tenant that isn't there is an error too.
            assert!(remove("alice").is_err());
        });
    }

    #[test]
    fn a_created_tenant_has_the_user_schema() {
        with_home(|_| {
            let path = create("alice").unwrap();
            let conn = Connection::open(&path).unwrap();
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='collection'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1);
        });
    }

    #[test]
    fn list_ignores_sidecars_and_snapshots() {
        with_home(|home| {
            create("alice").unwrap();
            let dir = home.join("tenants");
            std::fs::write(dir.join("alice.sqlite-wal"), b"").unwrap();
            std::fs::write(dir.join("alice.sqlite-shm"), b"").unwrap();
            std::fs::write(dir.join("alice.sqlite.bak"), b"").unwrap();
            std::fs::write(dir.join("README"), b"").unwrap();
            assert_eq!(list().unwrap(), vec!["alice"]);
        });
    }

    /// The property the whole approach exists to preserve: the catalog is
    /// ONE file, at an unchanged path, attached identically by every
    /// tenant — not a copy per tenant.
    #[test]
    fn every_tenant_attaches_the_same_unchanged_catalog() {
        with_home(|home| {
            seed_catalog();
            let shared = shared_db_path().unwrap();
            assert_eq!(shared, home.join("shared.sqlite"));

            let alice = create("alice").unwrap();
            let bob = create("bob").unwrap();

            let a = connect_user(&alice, &shared).unwrap();
            let b = connect_user(&bob, &shared).unwrap();

            // Both see the catalog, unqualified, through the temp views.
            assert!(catalog::card_exists(&a, "sv3pt5-1").unwrap());
            assert!(catalog::card_exists(&b, "sv3pt5-1").unwrap());

            // And both resolved `shared` to the same file on disk.
            let a_file: String = a
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name = 'shared'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let b_file: String = b
                .query_row(
                    "SELECT file FROM pragma_database_list WHERE name = 'shared'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(a_file, b_file);
            assert_eq!(
                std::fs::canonicalize(&a_file).unwrap(),
                std::fs::canonicalize(&shared).unwrap()
            );

            // There is exactly one catalog on disk — no per-tenant copy.
            assert!(!home.join("tenants").join("shared.sqlite").exists());
        });
    }

    /// Provisioning is isolation: a write through one tenant's connection
    /// is invisible to another's.
    #[test]
    fn tenants_do_not_see_each_others_collections() {
        with_home(|_| {
            seed_catalog();
            let shared = shared_db_path().unwrap();
            let a = connect_user(&create("alice").unwrap(), &shared).unwrap();
            let b = connect_user(&create("bob").unwrap(), &shared).unwrap();

            a.execute(
                "INSERT INTO binders (name, created_at, updated_at) \
                 VALUES ('Alice''s binder', '2026-08-07', '2026-08-07')",
                [],
            )
            .unwrap();

            let seen_by_b: i64 = b
                .query_row("SELECT count(*) FROM binders", [], |r| r.get(0))
                .unwrap();
            assert_eq!(seen_by_b, 0, "bob must not see alice's binder");
        });
    }

    /// The production migration and its rollback, both actually run, with
    /// the collection's rows checked at every step.
    #[test]
    fn adopt_then_revert_round_trips_the_data() {
        with_home(|home| {
            seed_catalog();
            let shared = shared_db_path().unwrap();
            let legacy = home.join("collection.sqlite");

            // Stand in for the prod database: pre-tenants location, with
            // real rows, written through a WAL connection.
            {
                let conn = connect_user(&legacy, &shared).unwrap();
                conn.execute(
                    "INSERT INTO collection (printing_id, acquired_at, source) \
                     VALUES ('sv3pt5-1-normal', '2026-08-07', 'manual_id')",
                    [],
                )
                .unwrap();
                assert!(sidecar(&legacy, "-wal").exists(), "expected a live WAL");
            }

            // Before adopting, the app refuses to open anything.
            assert!(crate::paths::user_db_path("collection").is_err());

            let moved = adopt("collection").unwrap();
            assert_eq!(moved, home.join("tenants").join("collection.sqlite"));
            assert!(!legacy.exists(), "legacy database must be gone");
            assert!(!sidecar(&legacy, "-wal").exists(), "stale WAL left behind");
            assert_eq!(list().unwrap(), vec!["collection"]);
            assert_eq!(crate::paths::user_db_path("collection").unwrap(), moved);

            // The row survived the move, and the catalog still attaches.
            {
                let conn = connect_user(&moved, &shared).unwrap();
                let n: i64 = conn
                    .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(n, 1);
                assert!(catalog::card_exists(&conn, "sv3pt5-1").unwrap());
            }

            // Rollback: back to exactly where it started.
            let back = revert("collection").unwrap();
            assert_eq!(back, legacy);
            assert!(legacy.exists());
            assert!(!moved.exists());
            assert_eq!(list().unwrap(), Vec::<String>::new());
            let conn = connect_user(&legacy, &shared).unwrap();
            let n: i64 = conn
                .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1, "rollback must not lose the collection");
        });
    }

    /// Moving a database out from under a server that is reading it leaves
    /// that server writing to an unlinked inode. The TRUNCATE checkpoint is
    /// what catches it — an in-flight reader makes it report busy.
    #[test]
    fn adopt_refuses_while_the_database_is_being_read() {
        with_home(|home| {
            seed_catalog();
            let legacy = home.join("collection.sqlite");
            let reader = connect_user(&legacy, &shared_db_path().unwrap()).unwrap();
            reader
                .execute_batch("BEGIN; SELECT count(*) FROM collection;")
                .unwrap();

            let err = adopt("collection").unwrap_err().to_string();
            assert!(err.contains("open in another process"), "unexpected: {err}");
            assert!(legacy.exists(), "the database must not have moved");

            reader.execute_batch("COMMIT").unwrap();
            drop(reader);
            adopt("collection").unwrap();
        });
    }

    /// Prod's data directory has a `.collection.sqlite-litestream` beside
    /// the database. If it does not travel with the file, the sidecar sees
    /// a brand-new database against an S3 prefix that already has history.
    #[test]
    fn adopt_takes_the_litestream_directory_with_it() {
        with_home(|home| {
            seed_catalog();
            let legacy = home.join("collection.sqlite");
            connect_user(&legacy, &shared_db_path().unwrap()).unwrap();

            let ls = home.join(".collection.sqlite-litestream");
            std::fs::create_dir_all(ls.join("ltx/0")).unwrap();
            std::fs::write(ls.join("ltx/0/0001-0001.ltx"), b"ltx").unwrap();

            adopt("collection").unwrap();

            assert!(!ls.exists(), "Litestream state left at the old location");
            let moved = home.join("tenants").join(".collection.sqlite-litestream");
            assert_eq!(
                std::fs::read(moved.join("ltx/0/0001-0001.ltx")).unwrap(),
                b"ltx"
            );

            // And back again on rollback.
            revert("collection").unwrap();
            assert!(!moved.exists());
            assert!(ls.join("ltx/0/0001-0001.ltx").exists());
        });
    }

    /// A removed tenant must not leave replication state for a later tenant
    /// of the same name to inherit.
    #[test]
    fn remove_takes_the_litestream_directory_with_it() {
        with_home(|home| {
            create("alice").unwrap();
            let ls = home.join("tenants").join(".alice.sqlite-litestream");
            std::fs::create_dir_all(&ls).unwrap();
            std::fs::write(ls.join("txid.db"), b"x").unwrap();

            remove("alice").unwrap();
            assert!(!ls.exists());
        });
    }

    #[test]
    fn adopt_refuses_to_overwrite_an_existing_tenant() {
        with_home(|home| {
            create("collection").unwrap();
            std::fs::write(home.join("collection.sqlite"), b"").unwrap();
            let err = adopt("collection").unwrap_err().to_string();
            assert!(err.contains("refusing to move"), "unexpected: {err}");
            // Neither file moved.
            assert!(home.join("collection.sqlite").exists());
            assert!(home.join("tenants").join("collection.sqlite").exists());
        });
    }

    #[test]
    fn adopt_reports_a_missing_database_rather_than_inventing_one() {
        with_home(|_| {
            assert!(adopt("collection").is_err());
            assert!(!tenant_db_path("collection").unwrap().exists());
        });
    }

    #[test]
    fn provisioning_rejects_a_traversing_name() {
        with_home(|home| {
            assert!(create("../escape").is_err());
            assert!(remove("../escape").is_err());
            assert!(adopt("../escape").is_err());
            assert!(!home.join("escape.sqlite").exists());
        });
    }
}
