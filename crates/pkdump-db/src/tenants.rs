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
//! [`adopt`] and [`revert`] move a data directory laid out before `tenants/`
//! existed into that directory, and they address files by handle because that
//! is what those files are named. [`migrate`] and [`unmigrate`] are the step
//! *after*: they take handle-named databases already under `tenants/` and put
//! them on opaque ids, registry row and all. Two migrations, each with its own
//! rollback, because a box can be at either point.
//!
//! [`resolve`] is what single-tenant mode opens, and it is deliberately NOT a
//! gate: a data directory that has not been migrated is served as it is,
//! because production runs single-tenant and a required migration is how the
//! previous epic took it down (`pd-uoph`). What it refuses to do is come up
//! *empty* — every branch either finds real bytes or says exactly which
//! command makes them exist.
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
use crate::paths::{
    legacy_user_db_path, tenant_db_path, tenant_db_path_for_id, tenants_dir, validate_database_id,
    validate_tenant_name,
};
use crate::registry::{self, User, UserState};
use crate::schema_version::{self, Database, SchemaState};

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

/// How single-tenant mode arrived at the database it is about to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Storage {
    /// A registry row named it: `tenants/<database_id>.sqlite`. The model
    /// this epic exists to reach.
    Registered(User),
    /// No registry row — a handle-named database from before [`migrate`].
    /// Served exactly as it is; the caller is expected to say so out loud.
    Unmigrated,
}

/// The collection single-tenant mode serves, and how it was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub path: PathBuf,
    pub storage: Storage,
}

impl Collection {
    /// Whether this data directory still needs [`migrate`] run against it.
    pub fn is_unmigrated(&self) -> bool {
        self.storage == Storage::Unmigrated
    }
}

/// The collection database `pkdump serve` (and every CLI command) opens for
/// `handle` when tenant resolution is off — which is production.
///
/// # This is not a migration gate, and that is the whole design
///
/// The previous epic shipped a required migration and made the app refuse to
/// start until it had been run. The refusal was correct in isolation and it
/// took production down on the first automated deploy, because nobody had
/// ever started the new binary against a volume the old one made (`pd-uoph`).
/// So a data directory whose databases are still named by handle is served,
/// not refused: prod keeps running across the upgrade and migrates when its
/// operator chooses to.
///
/// What is *not* tolerated is coming up empty. A collection silently reading
/// as zero rows is the worst outcome available to this project, so every
/// branch below either resolves to bytes that exist or fails naming the one
/// command that would create them:
///
/// 1. An **active registry row** — `tenants/<database_id>.sqlite`. The
///    migrated state, and the only one multi-tenant resolution can reach.
/// 2. **`tenants/<handle>.sqlite`** — pre-[`migrate`], served as-is.
/// 3. **`$PKDUMP_HOME/<handle>.sqlite`** — pre-[`adopt`]; the `pd-gckl`
///    refusal, unchanged, because an un-adopted database being shadowed by an
///    empty new one is the failure that guard was written for.
/// 4. **Nothing anywhere, and nothing registered** — a genuinely fresh data
///    directory. The handle is registered and an id minted, so a new install
///    is born on the two-identifier model rather than needing a migration it
///    could have skipped.
/// 5. **Nothing for this handle, but this data directory holds other users** —
///    an error naming `pkdump tenant create`. This is the one behaviour change
///    an operator can notice, and it removes a silent-empty that exists today:
///    a typo in `$PKDUMP_USER` currently provisions an empty collection under
///    the typo and serves it.
pub fn resolve(handle: &str) -> Result<Collection> {
    validate_tenant_name(handle)?;
    let conn = registry::open()?;

    // `lookup` is active-only by construction — a detached row keeps its
    // holder's real handle, so "is this handle live?" is a question only the
    // registry's own predicate can answer, and it does.
    if let Some(user) = registry::lookup(&conn, handle)? {
        let path = tenant_db_path_for_id(&user.database_id)?;
        if !path.exists() {
            // The registry and the disk disagree. If what is actually there
            // is a pre-`tenants/` database, say so in the terms the operator
            // already has a runbook for; otherwise report the drift plainly.
            let legacy = legacy_user_db_path(handle)?;
            if legacy.exists() {
                return Err(unadopted(handle, &legacy)?);
            }
            return Err(DbError::Env(format!(
                "the user registry says tenant {handle:?} is served from database \
                 {} but {} does not exist. Nothing was created: an empty collection \
                 in its place is not a recovery. Restore it (deploy/RESTORE.md), or \
                 roll the migration back with `pkdump tenant unmigrate`.",
                user.database_id,
                path.display()
            )));
        }
        return Ok(Collection {
            path,
            storage: Storage::Registered(user),
        });
    }

    let by_handle = tenant_db_path(handle)?;
    if by_handle.exists() {
        return Ok(Collection {
            path: by_handle,
            storage: Storage::Unmigrated,
        });
    }

    let legacy = legacy_user_db_path(handle)?;
    if legacy.exists() {
        return Err(unadopted(handle, &legacy)?);
    }

    // Nothing for this handle. Whether that is a fresh install or a mistake
    // is answered by whether this data directory holds anyone at all.
    if registry::list(&conn)?.is_empty() && databases_on_disk()?.is_empty() {
        // Through [`create`], so the row and the file land together and an
        // active registry row always names a database that is there — the
        // invariant the first branch above relies on to call a missing one a
        // fault rather than a fresh start.
        drop(conn);
        let tenant = create(handle)?;
        return Ok(Collection {
            path: tenant.path,
            storage: Storage::Registered(tenant.user),
        });
    }
    Err(DbError::NotFound(format!(
        "no collection for tenant {handle:?} in {}. This data directory holds other \
         users, so an empty one is not being created under that name — check \
         $PKDUMP_USER, or run `pkdump tenant create {handle}` if that is really a \
         new user. `pkdump tenant list` shows who is registered.",
        tenants_dir()?.display()
    )))
}

/// The `pd-gckl` refusal: a collection still at the pre-`tenants/` location.
fn unadopted(handle: &str, legacy: &Path) -> Result<DbError> {
    Ok(DbError::Env(format!(
        "collection database for tenant {handle:?} is still at the pre-tenants \
         location {} and has not been adopted into {}. \
         Run `pkdump tenant adopt {handle}` (see deploy/TENANTS.md).",
        legacy.display(),
        tenants_dir()?.display(),
    )))
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
/// row survives, still carrying the person's real handle, so the bytes stay
/// attributable — and the handle is immediately free for someone else, who
/// will get their own `database_id`, and therefore their own file and their
/// own replica prefix. Both at once because the handle is unique only among
/// *active* rows; see `user_one_active_handle` in `schema_registry.sql`.
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

/// One database moved between the two ways of naming it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moved {
    /// The handle the database was, or is again, named by.
    pub handle: String,
    /// The id it was, or is now, named by.
    pub database_id: String,
    pub from: PathBuf,
    pub to: PathBuf,
}

/// The handle-named databases under `tenants/` that [`migrate`] would move.
///
/// A stem that is already a `database_id` is migrated; anything else is a
/// handle-named database from before this epic. Sorted, so a dry run and the
/// migration itself report in the same order.
pub fn migratable() -> Result<Vec<String>> {
    Ok(unregistered()?
        .into_iter()
        .filter(|stem| validate_database_id(stem).is_err())
        .collect())
}

/// Put every handle-named database under `tenants/` onto an opaque id:
/// register the handle, mint a `database_id`, and rename the file to match.
///
/// This is `pd-hqee`. Before it, a database was named by the handle whose
/// collection it held, so the handle was simultaneously a lookup key, a
/// filename and an S3 replica prefix. After it, the filename is a ULID that
/// only the registry issues, and the handle is one column of one row.
///
/// **Idempotent.** A second run finds no handle-named files and does nothing:
/// it neither duplicates a row nor renames a database that is already on its
/// id. That is a property of what it selects, not a flag it checks.
///
/// Each database is its own transaction — row inserted, file renamed, then
/// committed — so an interruption leaves the ones already done done and the
/// rest untouched. It refuses outright if a handle it would register is
/// already taken: a live user and a handle-named file of the same name is a
/// collision that must be looked at, not resolved by guessing.
///
/// **Stop the app and the Litestream sidecar first.** Each rename checkpoints
/// the database with `PRAGMA wal_checkpoint(TRUNCATE)` and refuses to proceed
/// if it reports busy, exactly as [`adopt`] does.
pub fn migrate() -> Result<Vec<Moved>> {
    let handles = migratable()?;
    let mut conn = registry::open()?;
    let mut moved = Vec::with_capacity(handles.len());
    for handle in handles {
        // A file whose name is not a handle the registry would ever have
        // issued is not something to invent an owner for.
        if let Err(e) = validate_tenant_name(&handle) {
            return Err(DbError::Env(format!(
                "cannot migrate {} — its name is neither a database id nor a handle \
                 ({e}). Move it out of {} and migrate the rest.",
                tenant_db_path_unchecked(&handle)?.display(),
                tenants_dir()?.display()
            )));
        }
        if let Some(existing) = registry::lookup(&conn, &handle)? {
            return Err(DbError::Conflict(format!(
                "handle {handle:?} is already registered to database {}, but a \
                 handle-named database is also sitting at {}. Two databases claim \
                 one user; resolve that by hand before migrating.",
                existing.database_id,
                tenant_db_path(&handle)?.display()
            )));
        }
        let from = tenant_db_path(&handle)?;
        let tx = conn.transaction()?;
        let user = registry::insert(&tx, &handle)?;
        let to = tenant_db_path_for_id(&user.database_id)?;
        relocate(&handle, &from, &to, Litestream::Reset)?;
        if let Err(e) = tx.commit() {
            // The row never landed, so the file must not stay under a name
            // nothing claims — that is precisely the unattributable database
            // this design exists to prevent.
            let _ = std::fs::rename(&to, &from);
            return Err(e.into());
        }
        moved.push(Moved {
            handle,
            database_id: user.database_id,
            from,
            to,
        });
    }
    Ok(moved)
}

/// The rollback for [`migrate`]: put every registered user's database back
/// under their handle and drop their registry row.
///
/// A build that predates this epic finds `tenants/<handle>.sqlite` and reads
/// the registry not at all, so putting the data directory back means both
/// halves — the rename *and* the row. The rename happens first, so the file is
/// attributable by its own name before the row that attributed it goes away
/// (see [`registry::unregister`]).
///
/// Detached users are left exactly as they are and reported by the caller:
/// their handle was released, so there is no name to give their database back,
/// and the build being rolled back to has no concept of them. Same
/// preconditions as [`migrate`] — stop the app and the sidecar.
pub fn unmigrate() -> Result<(Vec<Moved>, Vec<User>)> {
    let conn = registry::open()?;
    let (active, detached): (Vec<User>, Vec<User>) = registry::list(&conn)?
        .into_iter()
        .partition(|u| u.state == UserState::Active);

    let mut moved = Vec::new();
    for user in active {
        let from = tenant_db_path_for_id(&user.database_id)?;
        let to = tenant_db_path(&user.handle)?;
        if !from.exists() {
            return Err(DbError::NotFound(format!(
                "the registry says {:?} is served from {}, which does not exist — \
                 refusing to roll back a data directory that is already inconsistent",
                user.handle,
                from.display()
            )));
        }
        relocate(&user.handle, &from, &to, Litestream::Reset)?;
        if let Err(e) = registry::unregister(&conn, &user.database_id) {
            // Put it back rather than leaving a handle-named file that a row
            // still claims under its id.
            let _ = std::fs::rename(&to, &from);
            return Err(e);
        }
        moved.push(Moved {
            handle: user.handle,
            database_id: user.database_id,
            from,
            to,
        });
    }
    Ok((moved, detached))
}

/// Every `*.sqlite` directly under `tenants/`, by filename stem. Sorted.
///
/// The directory listing, with no opinion about who owns what — [`list`] and
/// [`unregistered`] are the two halves that do have one.
fn databases_on_disk() -> Result<Vec<String>> {
    let dir = tenants_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(&dir)
        .map_err(|e| DbError::Env(format!("reading {}: {e}", dir.display())))?;
    let mut stems: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|f| f.strip_suffix(".sqlite").map(str::to_string))
        .collect();
    stems.sort();
    Ok(stems)
}

/// Where a stem sits under `tenants/`, without asking whether it is a name
/// anything would have issued. Only for naming a file in an error message
/// about that file being unnameable.
fn tenant_db_path_unchecked(stem: &str) -> Result<PathBuf> {
    Ok(tenants_dir()?.join(format!("{stem}.sqlite")))
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

/// A registered user and the schema version their collection database
/// carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantSchema {
    /// The user and the file they map to, exactly as [`list`] reports them.
    pub tenant: Tenant,
    /// `PRAGMA user_version` read off that user's database — `None` when the
    /// file the registry names is not on this box. A row with no bytes has no
    /// version, and that is drift to show beside `present: false` rather than
    /// an error that would hide every other row in the report.
    pub version: Option<i64>,
}

impl TenantSchema {
    /// Where this user's database stands relative to this build, or `None`
    /// for a database that is not here to have a version.
    pub fn state(&self) -> Option<SchemaState> {
        self.version.map(|v| Database::User.state_of(v))
    }
}

/// Every registered user with the schema version of their collection
/// database — the answer to "which of my N databases are behind" (pd-enje).
///
/// One database per user means they can legitimately differ: a user created
/// today carries this build's version, one restored from a replica carries
/// whatever it had when it was replicated, and one left over from before the
/// gate carries 0. Nothing could report that until now.
///
/// Reads each file's header without applying schema and without the gate
/// (see [`schema_version::version_of_file`]), so a database this build would
/// *refuse to open* is still listed, with the version that makes it
/// refusable. Reporting drift that stops the server is the point; failing
/// the same way the server does would not be a report.
pub fn versions() -> Result<Vec<TenantSchema>> {
    list()?
        .into_iter()
        .map(|tenant| {
            let version = tenant
                .present
                .then(|| schema_version::version_of_file(&tenant.path))
                .transpose()?;
            Ok(TenantSchema { tenant, version })
        })
        .collect()
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
    relocate(name, &from, &to, Litestream::Carry)
}

/// The rollback for [`adopt`]: move a tenant's database back to the
/// pre-`tenants/` location, where a build without the tenant layout will
/// find it. Same mechanics, opposite direction.
pub fn revert(name: &str) -> Result<PathBuf> {
    let from = tenant_db_path(name)?;
    let to = legacy_user_db_path(name)?;
    relocate(name, &from, &to, Litestream::Carry)
}

/// What a move does with Litestream's per-database state directory — the LTX
/// cache and txid it keeps beside the database (`.<db>-litestream`).
///
/// This is not a detail, it is the difference between a backup and a unit that
/// merely looks active. `deploy/litestream.yml` runs in directory mode, where
/// **the replica prefix is derived from the filename**. So whether the state
/// may travel with the file is decided by one question: does this move change
/// the name?
enum Litestream {
    /// The filename is unchanged, so the state still describes the prefix it
    /// was built against and must go with the database — left behind, the
    /// sidecar would treat a relocated file as brand new against a prefix that
    /// already holds months of history.
    Carry,
    /// The filename changes, so the replica prefix changes with it, and the
    /// state describes a prefix this database no longer writes to. It is
    /// removed rather than moved.
    ///
    /// **This is the `pd-1717` lesson, paid for on production.** After the
    /// `pd-gckl` migration the state directory was carried across a prefix
    /// change; Litestream came up active, logged "snapshot complete", and
    /// replicated nothing — `txid.replica` stuck at `0000000000000000` while
    /// `txid.db` climbed — because its LTX history began mid-stream and it
    /// could not catch an empty new prefix up from files it no longer had. It
    /// errored with "LTX file is missing" and sat there. Removing the
    /// directory and restarting was the fix, so a rename does it up front.
    ///
    /// Only derived state is destroyed: the database is untouched, and the old
    /// prefix keeps every object it had, which is what the pre-cutover half of
    /// the recovery window is (`deploy/TENANTS.md`).
    Reset,
}

/// Checkpoint `from`, then `rename(2)` it to `to`. Shared by every migration
/// here so no rollback can drift from the migration it undoes.
fn relocate(name: &str, from: &Path, to: &Path, litestream: Litestream) -> Result<PathBuf> {
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
    // `.<db>-litestream` directory beside the database — prod has one. Whether
    // it may travel is the question [`Litestream`] documents; getting it wrong
    // is a silently dead backup either way.
    match litestream {
        Litestream::Carry => {
            if from_ls.exists()
                && let Err(e) = std::fs::rename(&from_ls, &to_ls)
            {
                // Put the database back so the operator is left with the
                // layout they started from rather than a half-moved one.
                let _ = std::fs::rename(to, from);
                return Err(DbError::Env(format!(
                    "moved {} but could not move its Litestream directory {} to {}: {e} \
                     (the database was moved back)",
                    from.display(),
                    from_ls.display(),
                    to_ls.display()
                )));
            }
        }
        Litestream::Reset => {
            // `to_ls` cannot exist — the guard above refuses to move onto it —
            // so there is exactly one directory here, the one belonging to the
            // name being left behind.
            if from_ls.exists()
                && let Err(e) = std::fs::remove_dir_all(&from_ls)
            {
                let _ = std::fs::rename(to, from);
                return Err(DbError::Env(format!(
                    "moved {} but could not clear its Litestream state directory \
                     {}: {e}. Leaving it would point the sidecar at the prefix this \
                     database no longer writes to, which replicates nothing while \
                     reporting healthy (pd-1717). The database was moved back.",
                    from.display(),
                    from_ls.display()
                )));
            }
        }
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

    /// Idempotent, so a test that stands up two tenants can call it per
    /// tenant without the second one failing on the catalog's primary key.
    fn seed_catalog() {
        let conn = open_shared(&shared_db_path().unwrap()).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO sets (set_code, name, series) \
             VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO cards (card_id, set_code, number, number_sortable, name) \
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
            // Both halves at once, which is what the partial index buys: the
            // handle is free, AND the surviving row still carries her real
            // name, so the file left on disk is attributable to whom it
            // belonged without anything having to parse the handle.
            assert_eq!(detached.user.handle, "alice");
            assert!(detached.user.retired_at.is_some(), "{detached:?}");

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

            // Two rows, both named alice, exactly one live — and the retired
            // one still points at the file holding the first alice's binder.
            // That is what makes those bytes restorable rather than orphaned.
            let all = list().unwrap();
            assert_eq!(all.len(), 2);
            assert!(all.iter().all(|t| t.user.handle == "alice"), "{all:?}");
            assert_eq!(handles(), vec!["alice"]);
            let retired = all
                .iter()
                .find(|t| t.user.state == UserState::Detached)
                .expect("the first alice's row must survive");
            assert_eq!(retired.user.database_id, first.user.database_id);
            assert_eq!(retired.path, first.path);
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
            assert!(resolve("collection").is_err());

            let moved = adopt("collection").unwrap();
            assert_eq!(moved, home.join("tenants").join("collection.sqlite"));
            assert!(!legacy.exists(), "legacy database must be gone");
            assert!(!sidecar(&legacy, "-wal").exists(), "stale WAL left behind");
            // The registry knows nothing of it — a handle-named database is
            // exactly the drift `unregistered` exists to surface (pd-hqee
            // migrates these onto opaque ids).
            assert_eq!(list().unwrap(), Vec::new());
            assert_eq!(unregistered().unwrap(), vec!["collection"]);
            assert_eq!(resolve("collection").unwrap().path, moved);

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

    /// The drift report: three users at three different versions, each read
    /// off their own file. One database per user means they can legitimately
    /// differ, and until this there was no way to see it.
    #[test]
    fn versions_reports_each_users_own_schema_version() {
        with_home(|_| {
            let known = Database::User.version();
            let current = create("current").unwrap();
            let behind = create("behind").unwrap();
            let ahead = create("ahead").unwrap();
            set_version(&behind.path, 0);
            set_version(&ahead.path, known + 1);

            // Creation order, as `list` reports it.
            let reported = versions().unwrap();
            assert_eq!(
                reported,
                vec![
                    TenantSchema {
                        tenant: current,
                        version: Some(known),
                    },
                    TenantSchema {
                        tenant: behind,
                        version: Some(0),
                    },
                    TenantSchema {
                        tenant: ahead,
                        version: Some(known + 1),
                    },
                ]
            );
            let states: Vec<Option<SchemaState>> =
                reported.iter().map(TenantSchema::state).collect();
            assert_eq!(
                states,
                vec![
                    Some(SchemaState::Current),
                    Some(SchemaState::Behind),
                    Some(SchemaState::Ahead),
                ]
            );
        });
    }

    /// A user this build refuses to OPEN must still be REPORTED — that user
    /// is the whole reason an operator is running this command. A report that
    /// failed the same way the server did would name nobody.
    #[test]
    fn a_user_from_the_future_is_reported_not_refused() {
        with_home(|_| {
            let alice = create("alice").unwrap();
            create("bob").unwrap();
            let ahead = Database::User.version() + 1;
            set_version(&alice.path, ahead);

            assert!(
                open_user(&alice.path).is_err(),
                "the fixture must be one this build refuses to open"
            );

            let reported = versions().unwrap();
            assert_eq!(reported.len(), 2, "the refusable user must still appear");
            assert_eq!(reported[0].version, Some(ahead));
            assert_eq!(reported[0].state(), Some(SchemaState::Ahead));
            // And reporting did not "fix" what it read: the operator's next
            // move is to run the newer build, which must find its database
            // exactly as it left it.
            assert_eq!(
                schema_version::version_of_file(&alice.path).unwrap(),
                ahead
            );
        });
    }

    /// A registry row whose database is not on this box has no version to
    /// report — and must not take the whole listing down with it. Drift shows
    /// up as a blank column next to `(DATABASE MISSING)`, not as an error
    /// that hides every other row.
    #[test]
    fn a_missing_database_reports_no_version_rather_than_failing() {
        with_home(|_| {
            let alice = create("alice").unwrap();
            create("bob").unwrap();
            std::fs::remove_file(&alice.path).unwrap();

            let reported = versions().unwrap();
            assert_eq!(reported[0].version, None);
            assert_eq!(reported[0].state(), None);
            assert!(!reported[0].tenant.present);
            assert_eq!(reported[1].version, Some(Database::User.version()));
        });
    }

    /// Listing must not open a collection the way the app does: `open_user`
    /// applies the schema and stamps, so a report routed through it would
    /// silently migrate every database on the box just by being asked what
    /// version they are.
    #[test]
    fn reporting_versions_does_not_stamp_or_migrate() {
        with_home(|_| {
            let alice = create("alice").unwrap();
            set_version(&alice.path, 0);
            // Drop the tables too — an unversioned, schema-less file is what
            // an accidental `open_user` would visibly repair.
            Connection::open(&alice.path)
                .unwrap()
                .execute_batch("DROP TABLE collection")
                .unwrap();

            assert_eq!(versions().unwrap()[0].version, Some(0));

            let conn = Connection::open(&alice.path).unwrap();
            assert_eq!(schema_version::version(&conn).unwrap(), 0, "it stamped");
            let tables: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name = 'collection'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(tables, 0, "it applied the schema");
        });
    }

    /// Every user's version comes from that user's own file. A report that
    /// read one database N times would look perfectly healthy while hiding
    /// exactly the drift it exists to surface.
    #[test]
    fn no_user_reports_another_users_version() {
        with_home(|_| {
            let alice = create("alice").unwrap();
            let bob = create("bob").unwrap();
            set_version(&bob.path, 0);

            let reported = versions().unwrap();
            assert_eq!(reported[0].tenant.user.handle, "alice");
            assert_eq!(reported[0].version, Some(Database::User.version()));
            assert_eq!(reported[1].tenant.user.handle, "bob");
            assert_eq!(reported[1].version, Some(0));
            assert_ne!(alice.user.database_id, bob.user.database_id);
        });
    }

    /// `PRAGMA user_version = n` against a live WAL database, the way an
    /// older binary or a restore leaves one behind.
    fn set_version(path: &Path, version: i64) {
        Connection::open(path)
            .unwrap()
            .execute_batch(&format!("PRAGMA user_version = {version}"))
            .unwrap();
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

    // ── pd-hqee: existing tenants onto opaque ids ────────────────────────

    /// A pre-`pd-hqee` data directory: a handle-named collection under
    /// `tenants/`, with rows, a live WAL and Litestream state beside it — and
    /// no registry. This is the shape on the prod box, and the shape
    /// `deploy/setup.sh --test` seeds.
    fn old_layout(handle: &str) -> PathBuf {
        seed_catalog();
        let path = tenant_db_path(handle).unwrap();
        let conn = connect_user(&path, &shared_db_path().unwrap()).unwrap();
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source) \
             VALUES ('sv3pt5-1-normal', '2026-08-07', 'manual_id')",
            [],
        )
        .unwrap();
        let ls = litestream_dir(&path);
        std::fs::create_dir_all(ls.join("ltx/0")).unwrap();
        std::fs::write(ls.join("ltx/0/0001-0001.ltx"), b"ltx").unwrap();
        std::fs::write(ls.join("txid.db"), b"4e6").unwrap();
        path
    }

    fn rows(path: &Path) -> i64 {
        connect_user(path, &shared_db_path().unwrap())
            .unwrap()
            .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
            .unwrap()
    }

    /// **The bead's first acceptance criterion, and prod's actual state.**
    /// An existing handle-named database gets a registry row and an opaque
    /// id, the file is renamed to match, and the rows survive.
    #[test]
    fn migrate_registers_and_renames_an_existing_database() {
        with_home(|home| {
            let before = old_layout("collection");
            assert_eq!(migratable().unwrap(), vec!["collection"]);

            let moved = migrate().unwrap();
            assert_eq!(moved.len(), 1);
            let m = &moved[0];
            assert_eq!(m.handle, "collection");
            assert_eq!(m.from, before);

            // The file is named by the minted id, and the handle appears
            // nowhere in it.
            let id = &m.database_id;
            assert_eq!(m.to, home.join("tenants").join(format!("{id}.sqlite")));
            assert!(m.to.exists() && !before.exists());
            assert!(!m.to.to_string_lossy().contains("collection"));

            // The registry now joins the two, and the collection is intact.
            let t = lookup("collection").unwrap().unwrap();
            assert_eq!(t.user.database_id, *id);
            assert_eq!(t.user.state, UserState::Active);
            assert_eq!(t.path, m.to);
            assert!(t.present);
            assert_eq!(rows(&m.to), 1);

            // And nothing on disk is unattributable any more.
            assert_eq!(unregistered().unwrap(), Vec::<String>::new());
            assert_eq!(migratable().unwrap(), Vec::<String>::new());
        });
    }

    /// **Idempotent** — the bead asks for it by name. A second run neither
    /// duplicates a row nor moves a database that is already on its id.
    #[test]
    fn migrate_twice_changes_nothing_the_second_time() {
        with_home(|_| {
            old_layout("collection");
            old_layout("alice");
            let first = migrate().unwrap();
            assert_eq!(first.len(), 2);
            let after: Vec<_> = list().unwrap();

            let second = migrate().unwrap();
            assert!(
                second.is_empty(),
                "a second run moved something: {second:?}"
            );
            assert_eq!(list().unwrap(), after, "a second run touched the registry");
            for t in &after {
                assert!(t.path.exists(), "a second run lost {}", t.path.display());
                assert_eq!(rows(&t.path), 1);
            }
            // Two users, two ids, two files. Not one shared, not one lost.
            assert_eq!(after.len(), 2);
            assert_eq!(
                after
                    .iter()
                    .map(|t| t.user.database_id.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                2
            );
        });
    }

    /// **The rollback, exercised** — the bead's second acceptance criterion.
    /// Every file goes back under its handle, every row goes away, and the
    /// data is still there at both ends. That is what a build predating the
    /// registry needs to find.
    #[test]
    fn unmigrate_puts_every_database_back_under_its_handle() {
        with_home(|home| {
            old_layout("collection");
            old_layout("alice");
            migrate().unwrap();
            assert!(!home.join("tenants").join("collection.sqlite").exists());

            let (moved, detached) = unmigrate().unwrap();
            assert_eq!(moved.len(), 2);
            assert!(detached.is_empty());

            for handle in ["collection", "alice"] {
                let path = tenant_db_path(handle).unwrap();
                assert!(path.exists(), "{handle} did not come back");
                assert_eq!(rows(&path), 1, "{handle} lost its collection");
            }
            // The registry has nothing left to say, and the handles are free
            // — a rollback is not a detach.
            assert_eq!(list().unwrap(), Vec::new());
            assert!(!exists("collection").unwrap());
            // Which is exactly the state migrate started from, so it can be
            // run again.
            assert_eq!(migratable().unwrap(), vec!["alice", "collection"]);
        });
    }

    /// A rename changes the filename, and the filename **is** the replica
    /// prefix (`deploy/litestream.yml` runs in directory mode). Carrying the
    /// state directory across that is what left prod replicating nothing at
    /// `txid.replica=0` while reporting healthy — so a rename clears it, in
    /// both directions. `pd-1717`.
    #[test]
    fn migrating_clears_litestream_state_rather_than_carrying_it() {
        with_home(|home| {
            let before = old_layout("collection");
            let old_state = litestream_dir(&before);
            assert!(old_state.join("txid.db").exists());

            let m = migrate().unwrap().pop().unwrap();
            assert!(
                !old_state.exists(),
                "the old prefix's state was left beside the tenants dir"
            );
            assert!(
                !litestream_dir(&m.to).exists(),
                "the state was carried onto the new name — the sidecar would come \
                 up mid-stream against an empty prefix and stall (pd-1717)"
            );
            // Nothing else was collected on the way past.
            assert!(home.join("shared.sqlite").exists());

            // And the same on the way back.
            std::fs::create_dir_all(litestream_dir(&m.to).join("ltx")).unwrap();
            unmigrate().unwrap();
            assert!(!litestream_dir(&m.to).exists());
            assert!(!litestream_dir(&before).exists());
        });
    }

    /// The same refusal `adopt` has, for the same reason: renaming a database
    /// out from under a process that is reading it leaves that process writing
    /// to an unlinked inode. Stop the app first.
    #[test]
    fn migrate_refuses_while_the_database_is_being_read() {
        with_home(|_| {
            let path = old_layout("collection");
            // A serving app, which is what this refusal is aimed at: frames in
            // the WAL and a read transaction open over them. (An idle handle
            // on a checkpointed database is invisible to `wal_checkpoint`, as
            // `adopt`'s docs already say — stop the services regardless.)
            let reader = connect_user(&path, &shared_db_path().unwrap()).unwrap();
            reader
                .execute(
                    "INSERT INTO collection (printing_id, acquired_at, source) \
                     VALUES ('sv3pt5-1-holofoil', '2026-08-08', 'manual_id')",
                    [],
                )
                .unwrap();
            reader
                .execute_batch("BEGIN; SELECT count(*) FROM collection;")
                .unwrap();

            let err = migrate().unwrap_err().to_string();
            assert!(err.contains("open in another process"), "unexpected: {err}");
            // Nothing moved, and no half-written registry row was left behind
            // claiming a database that is still under its handle.
            assert!(path.exists());
            assert_eq!(list().unwrap(), Vec::new());

            reader.execute_batch("COMMIT").unwrap();
            drop(reader);
            assert_eq!(migrate().unwrap().len(), 1);
        });
    }

    /// A handle-named database whose handle is already registered to some
    /// other database is two databases claiming one user. Refuse; do not pick.
    #[test]
    fn migrate_refuses_when_the_handle_is_already_registered() {
        with_home(|_| {
            let alice = create("alice").unwrap();
            old_layout("alice"); // ...and a stray handle-named one as well
            let err = migrate().unwrap_err().to_string();
            assert!(err.contains("already registered"), "unexpected: {err}");
            // Both are still there: nothing was renamed onto anything.
            assert!(alice.path.exists());
            assert!(tenant_db_path("alice").unwrap().exists());
            assert_eq!(list().unwrap().len(), 1);
        });
    }

    /// A file whose name is neither an id nor a handle gets reported, not
    /// registered under an invented name.
    #[test]
    fn migrate_refuses_a_database_that_is_not_named_like_anything() {
        with_home(|home| {
            std::fs::create_dir_all(home.join("tenants")).unwrap();
            let odd = home.join("tenants").join("Not A Handle.sqlite");
            std::fs::write(&odd, b"").unwrap();
            let err = migrate().unwrap_err().to_string();
            assert!(err.contains("neither a database id nor a handle"), "{err}");
            assert!(odd.exists());
            assert_eq!(list().unwrap(), Vec::new());
        });
    }

    // ── pd-hqee: what single-tenant startup opens ────────────────────────

    /// **Production's upgrade path.** The binary meets a data directory it has
    /// not migrated and serves the real collection, because a migration the
    /// app refuses to start without is how the last epic took prod down.
    #[test]
    fn an_unmigrated_data_dir_is_served_as_it_is() {
        with_home(|_| {
            let path = old_layout("collection");
            let c = resolve("collection").unwrap();
            assert_eq!(c.path, path);
            assert!(c.is_unmigrated());
            // The real rows — this resolving to an empty new file is the
            // failure this project can least afford.
            assert_eq!(rows(&c.path), 1);
        });
    }

    /// ...and after migrating, the same handle resolves to the id-named file
    /// with the same rows. The two halves of "prod survives the upgrade".
    #[test]
    fn a_migrated_data_dir_resolves_through_the_registry() {
        with_home(|_| {
            old_layout("collection");
            let m = migrate().unwrap().pop().unwrap();

            let c = resolve("collection").unwrap();
            assert_eq!(c.path, m.to);
            assert!(!c.is_unmigrated());
            assert_eq!(rows(&c.path), 1);
            let Storage::Registered(user) = c.storage else {
                panic!("expected a registered collection");
            };
            assert_eq!(user.database_id, m.database_id);
            assert_eq!(user.handle, "collection");

            // And the rollback restores the un-migrated answer, unchanged.
            unmigrate().unwrap();
            let back = resolve("collection").unwrap();
            assert!(back.is_unmigrated());
            assert_eq!(rows(&back.path), 1);
        });
    }

    /// A fresh data directory registers its handle, so a new install is born
    /// on the two-identifier model instead of needing a migration later.
    #[test]
    fn a_fresh_data_dir_registers_the_handle_it_serves() {
        with_home(|home| {
            let c = resolve("collection").unwrap();
            let Storage::Registered(user) = c.storage.clone() else {
                panic!("a fresh data dir should register its user");
            };
            assert_eq!(user.handle, "collection");
            assert_eq!(
                c.path,
                home.join("tenants")
                    .join(format!("{}.sqlite", user.database_id))
            );
            // Idempotent: the second call finds the row rather than minting a
            // second id and a second empty database.
            assert_eq!(resolve("collection").unwrap(), c);
            assert_eq!(list().unwrap().len(), 1);
        });
    }

    /// A handle nobody registered, in a data directory that plainly holds
    /// other people, must not quietly become a new empty collection. This is
    /// the one behaviour change an operator can notice, and it closes a
    /// silent-empty that a typo in `$PKDUMP_USER` reaches today.
    #[test]
    fn an_unknown_handle_does_not_become_an_empty_collection() {
        with_home(|home| {
            create("alice").unwrap();
            let err = resolve("collecton").unwrap_err().to_string();
            assert!(err.contains("pkdump tenant create"), "unhelpful: {err}");
            assert!(!home.join("tenants").join("collecton.sqlite").exists());
            assert_eq!(list().unwrap().len(), 1);
        });
    }

    /// The `pd-gckl` refusal, unchanged: a database still at the pre-tenants
    /// location is never shadowed by an empty new one.
    #[test]
    fn resolve_refuses_an_unadopted_legacy_database() {
        with_home(|home| {
            std::fs::write(home.join("collection.sqlite"), b"").unwrap();
            let err = resolve("collection").unwrap_err().to_string();
            assert!(err.contains("pkdump tenant adopt"), "unhelpful: {err}");
            assert_eq!(list().unwrap(), Vec::new());

            // Adopting clears it, and migrating then moves it onto an id.
            adopt("collection").unwrap();
            assert!(resolve("collection").unwrap().is_unmigrated());
            migrate().unwrap();
            assert!(!resolve("collection").unwrap().is_unmigrated());
        });
    }

    /// A registry row whose database is gone is a fault to surface with a way
    /// out in it — not a fresh empty collection, and not a crash.
    #[test]
    fn a_registered_user_whose_database_vanished_fails_with_a_way_out() {
        with_home(|_| {
            old_layout("collection");
            let m = migrate().unwrap().pop().unwrap();
            std::fs::rename(&m.to, m.to.with_extension("sqlite.moved-away")).unwrap();

            let err = resolve("collection").unwrap_err().to_string();
            assert!(err.contains(&m.database_id), "unhelpful: {err}");
            assert!(
                err.contains("unmigrate") || err.contains("RESTORE"),
                "{err}"
            );
            assert!(!m.to.exists(), "resolving created the missing database");
        });
    }

    #[test]
    fn provisioning_rejects_a_traversing_name() {
        with_home(|home| {
            assert!(create("../escape").is_err());
            assert!(detach("../escape").is_err());
            assert!(purge("../escape").is_err());
            assert!(adopt("../escape").is_err());
            assert!(resolve("../escape").is_err());
            assert!(!home.join("escape.sqlite").exists());
        });
    }
}
