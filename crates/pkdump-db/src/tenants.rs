//! Tenant provisioning — the operator surface over the user registry.
//!
//! A tenant used to *be* its database file: `tenants/<name>.sqlite` exists ⇒
//! the tenant exists. That made a tenant's name three things at once — the
//! value in a header, a filename, and an S3 replica prefix — which is what
//! made `pd-pm7b` possible: remove `alice`, create `alice` again, and the new
//! database lands under the old one's replica stream.
//!
//! So the filesystem is no longer the registry. [`crate::registry`] is, and
//! these four operations are it, seen from an operator's side:
//!
//! * [`create`] mints a `database_id` and writes the file that id names.
//! * [`list`] reads the registry back out — the mitigation for a `tenants/`
//!   directory that is no longer human-readable, and therefore not garnish.
//! * [`rename`] writes one column. No file moves, no replica moves, no
//!   history is disturbed. This is the capability the whole split buys.
//! * [`detach`] releases a handle and keeps the bytes.
//!
//! Hard deletion is [`purge`]: a separate, explicit act on a detached user,
//! not the default meaning of "remove".
//!
//! [`adopt`] and [`revert`] are untouched by all of this. They migrate a
//! data directory laid out before `tenants/` existed, and they address files
//! by handle because that is what those files are named — moving them onto
//! opaque ids is `pd-hqee`'s job, not theirs.
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
use crate::paths::{legacy_user_db_path, tenant_db_path, tenant_db_path_for_id, tenants_dir};
use crate::registry::{self, User, UserState};

/// Sidecar files SQLite keeps beside a database in WAL mode.
const WAL_SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// A registered user and the file their collection lives in — the two halves
/// of the map, joined, which is what an operator actually needs to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tenant {
    pub user: User,
    /// `tenants/<database_id>.sqlite`. Derived from the id, never the handle.
    pub path: PathBuf,
    /// Whether that file is actually on this box. A registered user whose
    /// database is missing is drift worth seeing, not an error to swallow.
    pub present: bool,
}

impl Tenant {
    fn of(user: User) -> Result<Self> {
        let path = tenant_db_path_for_id(&user.database_id)?;
        Ok(Tenant {
            present: path.exists(),
            path,
            user,
        })
    }
}

/// Register `handle` and provision their collection database.
///
/// The `database_id` is minted by the registry, so the filename is never
/// derived from the handle — creating `alice` a second time after she was
/// detached yields a different file under a different replica prefix, which
/// is the property this epic exists for.
///
/// Fails if the handle is already taken — provisioning is not idempotent on
/// purpose. "Create the user that is already there" is either a typo or a
/// second operator, and silently succeeding would make the second case look
/// like the first.
///
/// Row and file land together: the insert is held open in a transaction
/// until the database has been written, so a failure part-way through leaves
/// no user pointing at a file that does not exist.
pub fn create(handle: &str) -> Result<Tenant> {
    let mut conn = registry::open()?;
    let tx = conn.transaction()?;
    let user = registry::insert(&tx, handle)?;
    // `open_user` creates the parent directory and applies the user schema,
    // so a freshly created tenant is immediately usable.
    let tenant = Tenant::of(user)?;
    open_user(&tenant.path)?;
    tx.commit()?;
    Ok(Tenant {
        present: true,
        ..tenant
    })
}

/// Every registered user, detached ones included, in creation order, each
/// with the file they map to.
///
/// This reads the registry, not the directory. Under opaque ids a directory
/// listing says only how many collections exist, so this is the only way to
/// answer "whose is that file?" — see [`unregistered`] for the other
/// direction.
pub fn list() -> Result<Vec<Tenant>> {
    let conn = registry::open()?;
    registry::list(&conn)?.into_iter().map(Tenant::of).collect()
}

/// The tenant registered under `handle`, if the handle is live.
pub fn lookup(handle: &str) -> Result<Option<Tenant>> {
    let conn = registry::open()?;
    registry::lookup(&conn, handle)?.map(Tenant::of).transpose()
}

/// Whether `handle` names a live user.
pub fn exists(handle: &str) -> Result<bool> {
    Ok(lookup(handle)?.is_some())
}

/// Rename `from` to `to`. Nothing on disk moves.
///
/// The database keeps its `database_id`, so it keeps its filename, so it
/// keeps its Litestream prefix and every LTX file already under it. A user
/// changing their name costs one `UPDATE` and no replication history —
/// which was impossible while the name *was* the path.
pub fn rename(from: &str, to: &str) -> Result<Tenant> {
    let conn = registry::open()?;
    Tenant::of(registry::rename(&conn, from, to)?)
}

/// Release `handle`, keeping the database and its replica.
///
/// **This is what `pkdump tenant remove` now does.** Nothing is deleted: the
/// row survives under a retired handle so the bytes stay attributable, and
/// the handle is immediately free for someone else — who will get their own
/// `database_id`, and therefore their own file and their own replica prefix.
/// The retention window stops being the liability `pd-pm7b` made of it and
/// becomes a safety net.
///
/// Hard deletion is [`purge`], on the detached row, by `database_id`.
pub fn detach(handle: &str) -> Result<Tenant> {
    let conn = registry::open()?;
    Tenant::of(registry::detach(&conn, handle)?)
}

/// Destroy a detached user's collection: the database file, its WAL
/// sidecars, its Litestream bookkeeping directory, and the registry row.
/// Returns the row that was purged.
///
/// Addressed by `database_id`, not by handle, and only ever on a user
/// [`detach`] has already released. Both of those are deliberate: a purge is
/// the irreversible half of a removal, and it should be impossible to reach
/// by mistyping a live person's name.
///
/// This destroys the only copy on this box. The replica in S3 outlives it
/// (see `deploy/RESTORE.md`) but retention is finite — treat it as permanent.
///
/// The Litestream directory goes with it deliberately: leaving it behind
/// would hand a later database of the same name a predecessor's replication
/// state, which is the shape of the cross-tenant substitution bug the backup
/// spike found (`deep-dives/litestream-multi-db/RESULT.md` §2). Under opaque
/// ids no later database *has* the same name — this is the belt to that
/// braces.
pub fn purge(database_id: &str) -> Result<User> {
    let path = tenant_db_path_for_id(database_id)?;
    let conn = registry::open()?;
    let user = registry::find(&conn, database_id)?
        .ok_or_else(|| DbError::NotFound(format!("no user with database id {database_id:?}")))?;
    if user.state == UserState::Active {
        return Err(DbError::Conflict(format!(
            "user {:?} is still active — `pkdump tenant detach {}` first",
            user.handle, user.handle
        )));
    }
    // The file first. A row naming a database that is gone shows up in
    // `list` as missing; a database no row names is unattributable, and that
    // is the state this whole design exists to prevent.
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| DbError::Env(format!("removing {}: {e}", path.display())))?;
    }
    remove_sidecars(&path);
    let _ = std::fs::remove_dir_all(litestream_dir(&path));
    registry::delete(&conn, database_id)
}

/// Database files under `tenants/` that no registry row claims.
///
/// The registry is the source of truth, which means it can be *behind* the
/// disk: a data directory from before this epic (handle-named files, no rows
/// — `pd-hqee`'s migration), or a purge that failed after unlinking the file.
/// Either way an unattributable database is exactly the thing worth
/// reporting, so `list` shows these rather than pretending the directory
/// holds nothing else.
///
/// Returns filename stems, sorted. `-wal`/`-shm` sidecars and `.bak`
/// snapshots do not end in `.sqlite` and so are not mistaken for databases.
pub fn unregistered() -> Result<Vec<String>> {
    let dir = tenants_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let conn = registry::open()?;
    let known: std::collections::HashSet<String> = registry::list(&conn)?
        .into_iter()
        .map(|u| u.database_id)
        .collect();
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| DbError::Env(format!("reading {}: {e}", dir.display())))?;
    let mut stems: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_suffix(".sqlite").map(str::to_string))
        .filter(|stem| !known.contains(stem))
        .collect();
    stems.sort();
    Ok(stems)
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

    /// Handles of the live users, in registry order.
    fn handles() -> Vec<String> {
        list()
            .unwrap()
            .into_iter()
            .filter(|t| t.user.state == UserState::Active)
            .map(|t| t.user.handle)
            .collect()
    }

    #[test]
    fn create_list_detach_purge_round_trip() {
        with_home(|home| {
            assert_eq!(list().unwrap(), Vec::new());

            let alice = create("alice").unwrap();
            // The file is named by the minted id, NOT by the handle.
            assert_eq!(
                alice.path,
                home.join("tenants")
                    .join(format!("{}.sqlite", alice.user.database_id))
            );
            assert!(alice.present && alice.path.exists());
            assert!(!home.join("tenants").join("alice.sqlite").exists());

            create("bob").unwrap();
            assert_eq!(handles(), vec!["alice", "bob"]);
            assert!(exists("alice").unwrap());

            // Creating a user who already holds the handle is an error, not
            // a no-op — and it provisions nothing.
            assert!(create("alice").is_err());
            assert_eq!(list().unwrap().len(), 2);

            // Detaching frees the handle and keeps every byte.
            let detached = detach("alice").unwrap();
            assert_eq!(detached.user.state, UserState::Detached);
            assert_eq!(detached.user.database_id, alice.user.database_id);
            assert!(alice.path.exists(), "detach must not delete the database");
            assert_eq!(handles(), vec!["bob"]);
            assert!(!exists("alice").unwrap());
            assert!(detach("alice").is_err());

            // Purge is the second, explicit step — and it takes the file.
            let purged = purge(&alice.user.database_id).unwrap();
            assert_eq!(purged.database_id, alice.user.database_id);
            assert!(!alice.path.exists());
            assert_eq!(handles(), vec!["bob"]);
            assert_eq!(list().unwrap().len(), 1);
            assert!(purge(&alice.user.database_id).is_err());
        });
    }

    /// `remove` used to be a hard delete; it is now a detach. What it would
    /// have destroyed — the database and its replication state — survives,
    /// which is what turns the retention window from a liability into a
    /// safety net.
    #[test]
    fn detach_keeps_the_database_and_its_replication_state() {
        with_home(|_| {
            let alice = create("alice").unwrap();
            let ls = litestream_dir(&alice.path);
            std::fs::create_dir_all(&ls).unwrap();
            std::fs::write(ls.join("txid.db"), b"state").unwrap();

            detach("alice").unwrap();

            assert!(alice.path.exists(), "the collection must survive a detach");
            assert_eq!(std::fs::read(ls.join("txid.db")).unwrap(), b"state");

            // ...and purge, the explicit second step, takes both.
            purge(&alice.user.database_id).unwrap();
            assert!(!alice.path.exists());
            assert!(!ls.exists());
        });
    }

    #[test]
    fn purge_refuses_a_live_user() {
        with_home(|_| {
            let alice = create("alice").unwrap();
            let err = purge(&alice.user.database_id).unwrap_err().to_string();
            assert!(err.contains("still active"), "unhelpful error: {err}");
            assert!(alice.path.exists());
            assert!(exists("alice").unwrap());
        });
    }

    /// The acceptance criterion of `pd-zr9n`, and the whole point of the
    /// epic: a recycled handle cannot inherit its predecessor's storage.
    #[test]
    fn a_recreated_handle_gets_a_different_database() {
        with_home(|_| {
            let first = create("alice").unwrap();
            let payload = "INSERT INTO binders (name, created_at, updated_at) \
                           VALUES ('first alice', '2026-08-08', '2026-08-08')";
            Connection::open(&first.path)
                .unwrap()
                .execute(payload, [])
                .unwrap();

            detach("alice").unwrap();
            let second = create("alice").unwrap();

            assert_ne!(second.user.database_id, first.user.database_id);
            assert_ne!(
                second.path, first.path,
                "the second alice must not be handed the first one's file — \
                 and therefore not her replica prefix either"
            );
            // Both files exist; the new one is empty. Nothing was inherited.
            assert!(first.path.exists() && second.path.exists());
            let n: i64 = Connection::open(&second.path)
                .unwrap()
                .query_row("SELECT count(*) FROM binders", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "the new alice must not see the old alice's rows");
        });
    }

    /// The capability the split buys: a rename is one column.
    #[test]
    fn rename_moves_nothing_on_disk() {
        with_home(|_| {
            let before = create("alice").unwrap();
            // Stand in for replication state, which must not be disturbed.
            let ls = litestream_dir(&before.path);
            std::fs::create_dir_all(&ls).unwrap();
            std::fs::write(ls.join("txid.db"), b"state").unwrap();

            let after = rename("alice", "alicia").unwrap();

            assert_eq!(after.user.handle, "alicia");
            assert_eq!(after.user.database_id, before.user.database_id);
            assert_eq!(after.path, before.path, "the database must not move");
            assert!(after.present && after.path.exists());
            assert_eq!(std::fs::read(ls.join("txid.db")).unwrap(), b"state");

            assert!(!exists("alice").unwrap());
            assert_eq!(handles(), vec!["alicia"]);

            // A rename cannot smuggle in a name the registry would never
            // have issued, and a failed one changes nothing.
            assert!(rename("alicia", "../bob").is_err());
            assert_eq!(handles(), vec!["alicia"]);
        });
    }

    /// A user whose database could not be written must not be left in the
    /// registry: the row and the file land together or not at all.
    #[test]
    fn create_leaves_no_row_behind_when_the_database_cannot_be_written() {
        with_home(|home| {
            // `tenants` as a *file* makes creating anything under it fail.
            std::fs::write(home.join("tenants"), b"not a directory").unwrap();
            assert!(create("alice").is_err());
            assert!(!exists("alice").unwrap());
            assert_eq!(list().unwrap(), Vec::new());
        });
    }

    #[test]
    fn a_created_tenant_has_the_user_schema() {
        with_home(|_| {
            let path = create("alice").unwrap().path;
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

    /// A registered database is not "unregistered", and neither are the
    /// files that live beside it: sidecars, snapshots and stray notes.
    #[test]
    fn unregistered_reports_only_unclaimed_databases() {
        with_home(|home| {
            let alice = create("alice").unwrap();
            let dir = home.join("tenants");
            let stem = &alice.user.database_id;
            std::fs::write(dir.join(format!("{stem}.sqlite-wal")), b"").unwrap();
            std::fs::write(dir.join(format!("{stem}.sqlite-shm")), b"").unwrap();
            std::fs::write(dir.join(format!("{stem}.sqlite.bak")), b"").unwrap();
            std::fs::write(dir.join("README"), b"").unwrap();
            assert_eq!(unregistered().unwrap(), Vec::<String>::new());

            // A pre-registry, handle-named database is exactly what this is
            // for: real bytes that the registry cannot account for.
            std::fs::write(dir.join("collection.sqlite"), b"").unwrap();
            assert_eq!(unregistered().unwrap(), vec!["collection"]);

            // And a detached user's file is still claimed — the row that
            // names it is what keeps those bytes attributable.
            detach("alice").unwrap();
            assert_eq!(unregistered().unwrap(), vec!["collection"]);
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

            let a = connect_user(&alice.path, &shared).unwrap();
            let b = connect_user(&bob.path, &shared).unwrap();

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
            let a = connect_user(&create("alice").unwrap().path, &shared).unwrap();
            let b = connect_user(&create("bob").unwrap().path, &shared).unwrap();

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
            // The registry knows nothing of it — a handle-named database is
            // exactly the drift `unregistered` exists to surface (pd-hqee
            // migrates these onto opaque ids).
            assert_eq!(list().unwrap(), Vec::new());
            assert_eq!(unregistered().unwrap(), vec!["collection"]);
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
            assert_eq!(unregistered().unwrap(), Vec::<String>::new());
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

    #[test]
    fn adopt_refuses_to_overwrite_an_existing_tenant() {
        with_home(|home| {
            std::fs::create_dir_all(home.join("tenants")).unwrap();
            std::fs::write(home.join("tenants").join("collection.sqlite"), b"").unwrap();
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
            assert!(detach("../escape").is_err());
            assert!(purge("../escape").is_err());
            assert!(adopt("../escape").is_err());
            assert!(!home.join("escape.sqlite").exists());
        });
    }
}
