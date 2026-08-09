//! The one place a command asks which collection database it is working on.
//!
//! Every `pkdump` subcommand that touches a collection — `serve`, `db`,
//! `export`, `import`, `data` — resolves it through here, so they cannot
//! disagree about what `$PKDUMP_USER` means. [`pkdump_db::tenants::resolve`]
//! does the deciding; this adds the one thing a library should not, which is
//! saying out loud when a data directory is still on the pre-`pd-hqee` layout.
//!
//! That warning is not decoration. A handle-named database is served exactly
//! as it is — production must not need a migration to keep running — so the
//! *only* signal that the migration is still outstanding is this line.

use std::path::PathBuf;

use pkdump_db::tenants::{Collection, Storage};

/// The collection database for `$PKDUMP_USER`, warning on stderr if this data
/// directory has not been migrated onto opaque ids yet.
pub fn user_db() -> anyhow::Result<PathBuf> {
    Ok(resolve()?.path)
}

/// [`user_db`], keeping how it was resolved — for `serve`, which reports the
/// tenant it came up on.
pub fn resolve() -> anyhow::Result<Collection> {
    let handle = pkdump_db::current_user();
    let collection = pkdump_db::resolve_collection(&handle)?;
    if collection.is_unmigrated() {
        // stderr, not stdout: `pkdump export --json -o -` writes a collection
        // to stdout, and a warning in the middle of it would corrupt a backup.
        eprintln!(
            "warning: tenant {handle:?} is served from {}, which is named by handle \
             rather than by an opaque database id.\n\
             \x20        Run `pkdump tenant migrate` to move it onto one \
             (see deploy/TENANTS.md). Serving it as-is until you do.",
            collection.path.display()
        );
    }
    Ok(collection)
}

/// How a collection was reached, for `serve`'s startup line.
///
/// Under opaque ids the path alone no longer says whose collection it is, and
/// "which database did it actually open" is the first question every incident
/// in this area has started with — so the startup line answers it.
pub fn describe(collection: &Collection) -> String {
    match &collection.storage {
        Storage::Registered(user) => format!(
            "registered as {:?} -> database {}",
            user.handle, user.database_id
        ),
        Storage::Unmigrated => "named by handle, NOT YET MIGRATED".to_string(),
    }
}
