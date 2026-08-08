//! Filesystem paths for PokeDumpster's databases.
//!
//! Layout — one shared catalog, one collection database per tenant:
//!
//! ```text
//! $PKDUMP_HOME/                     # default ~/.pkdump
//!   shared.sqlite                   # the catalog: one copy, ATTACHed by every tenant
//!   tenants/
//!     collection.sqlite             # tenant `collection` (the original single user)
//!     <tenant>.sqlite               # one file per additional tenant
//! ```
//!
//! Tenant databases live in their own directory rather than beside the
//! catalog for two reasons, both load-bearing:
//!
//! 1. `shared.sqlite` must never be mistaken for a tenant. It is rebuildable
//!    from upstream and is deliberately NOT replicated; a flat layout makes
//!    "every `*.sqlite` in the data dir" mean two different kinds of file.
//! 2. It makes the whole set of tenant databases addressable as one glob,
//!    which is exactly the shape Litestream's `dir:` + `pattern:` + `watch:`
//!    mode wants (`deep-dives/litestream-multi-db/RESULT.md` §4). In that
//!    mode the replica path is derived from the filename, so distinct tenant
//!    names give distinct replica prefixes *by construction* — which is what
//!    forecloses the silent cross-tenant substitution the spike found (§2).
//!
//! The catalog path is unchanged by all of this: `shared.sqlite` sits at the
//! root of the data dir, and every tenant `ATTACH`es that same one file.
//!
//! Provisioning lives in [`crate::tenants`]; this module is only paths.

use std::path::PathBuf;

use crate::error::{DbError, Result};

const DEFAULT_USER: &str = "collection";

/// Directory under the data dir holding one database per tenant.
pub const TENANTS_DIR: &str = "tenants";

/// Maximum tenant-name length. A tenant name becomes a filename and an S3
/// replica-path component; 32 keeps both comfortable.
const MAX_TENANT_NAME: usize = 32;

/// The PokeDumpster data directory: `$PKDUMP_HOME` if set, else `$HOME/.pkdump`.
pub fn pkdump_home() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("PKDUMP_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")
        .map_err(|_| DbError::Env("neither PKDUMP_HOME nor HOME is set".into()))?;
    Ok(PathBuf::from(home).join(".pkdump"))
}

/// Path to the shared catalog database. One copy, shared by every tenant.
pub fn shared_db_path() -> Result<PathBuf> {
    Ok(pkdump_home()?.join("shared.sqlite"))
}

/// The directory holding every tenant's collection database.
pub fn tenants_dir() -> Result<PathBuf> {
    Ok(pkdump_home()?.join(TENANTS_DIR))
}

/// The active tenant: `$PKDUMP_USER` if set, else `collection`.
pub fn current_user() -> String {
    std::env::var("PKDUMP_USER").unwrap_or_else(|_| DEFAULT_USER.to_string())
}

/// Reject anything that would not be safe as both a filename and an S3
/// replica-path component: `[a-z0-9][a-z0-9_-]{0,31}`.
///
/// Deliberately narrow. A tenant name is not user-facing prose — it is an
/// identifier that ends up in a path on two systems at once:
///
/// * Lowercase only. On a case-insensitive filesystem (the macOS deployment
///   in `deploy/mac-setup.sh`) `Alice` and `alice` are the same file but two
///   different S3 prefixes — the replica and the database would disagree
///   about how many tenants exist.
/// * No `.`, `/` or `\`. Path traversal, and a name ending in `.sqlite`
///   would produce `foo.sqlite.sqlite`.
/// * No leading `-`, which reads as a flag to every CLI it is passed to.
pub fn validate_tenant_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(DbError::Env("tenant name is empty".into()));
    }
    if name.len() > MAX_TENANT_NAME {
        return Err(DbError::Env(format!(
            "tenant name {name:?} is longer than {MAX_TENANT_NAME} characters"
        )));
    }
    let first = name.as_bytes()[0];
    if !first.is_ascii_lowercase() && !first.is_ascii_digit() {
        return Err(DbError::Env(format!(
            "tenant name {name:?} must start with a lowercase letter or digit"
        )));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
    {
        return Err(DbError::Env(format!(
            "tenant name {name:?} contains {bad:?}; allowed: a-z 0-9 - _"
        )));
    }
    Ok(())
}

/// Path to a tenant's collection database. Validates the name; does not
/// touch the filesystem.
pub fn tenant_db_path(name: &str) -> Result<PathBuf> {
    validate_tenant_name(name)?;
    Ok(tenants_dir()?.join(format!("{name}.sqlite")))
}

/// Where a tenant's collection database lived before the `tenants/` layout:
/// `$PKDUMP_HOME/<name>.sqlite`, beside the catalog.
///
/// Only [`user_db_path`] (to refuse an unmigrated data dir) and
/// [`crate::tenants::adopt`] / [`crate::tenants::revert`] (to move the file,
/// and to move it back) have any business with this path.
pub fn legacy_user_db_path(name: &str) -> Result<PathBuf> {
    validate_tenant_name(name)?;
    Ok(pkdump_home()?.join(format!("{name}.sqlite")))
}

/// Path to the collection database the application should open for `name`.
///
/// Same as [`tenant_db_path`], plus one guard: if a database still sits at
/// the pre-`tenants/` location and has not been adopted, this fails rather
/// than returning a path that SQLite would happily create as an empty file.
/// A collection silently coming up empty is the failure mode this project
/// can least afford, and it is the one an unguarded path function produces.
/// The fix is a single command, and the error names it.
pub fn user_db_path(name: &str) -> Result<PathBuf> {
    let path = tenant_db_path(name)?;
    if !path.exists() {
        let legacy = legacy_user_db_path(name)?;
        if legacy.exists() {
            return Err(DbError::Env(format!(
                "collection database for tenant {name:?} is still at the pre-tenants \
                 location {} and has not been adopted into {}. \
                 Run `pkdump tenant adopt {name}` (see deploy/TENANTS.md).",
                legacy.display(),
                tenants_dir()?.display(),
            )));
        }
    }
    Ok(path)
}

/// Run `f` against a throwaway `$PKDUMP_HOME`.
///
/// `$PKDUMP_HOME` is process-global and `cargo test` is threaded, so every
/// test that sets it — here and in [`crate::tenants`] — has to share ONE
/// lock. Two locks would be no lock at all.
#[cfg(test)]
pub(crate) fn with_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    // SAFETY: serialised by LOCK, which every PKDUMP_HOME test goes through.
    unsafe { std::env::set_var("PKDUMP_HOME", dir.path()) };
    let out = f(dir.path());
    unsafe { std::env::remove_var("PKDUMP_HOME") };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_db_lives_under_tenants_dir() {
        with_home(|home| {
            assert_eq!(
                tenant_db_path("alice").unwrap(),
                home.join("tenants").join("alice.sqlite")
            );
            // The catalog does NOT move — every tenant attaches this one file.
            assert_eq!(shared_db_path().unwrap(), home.join("shared.sqlite"));
        });
    }

    #[test]
    fn tenant_names_are_validated() {
        for good in ["collection", "alice", "a", "tenant-2", "tenant_2", "9lives"] {
            assert!(validate_tenant_name(good).is_ok(), "{good} should be valid");
        }
        for bad in [
            "",
            "Alice",                             // case-insensitive filesystems collide
            "../escape",                         // traversal
            "a/b",                               // traversal
            "a\\b",                              // traversal, Windows-flavoured
            "-flag",                             // reads as a CLI flag
            "_leading",                          // must start alnum
            "has space",                         //
            "dot.sqlite",                        // would yield dot.sqlite.sqlite
            "ünïcode",                           //
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", // 33 chars
        ] {
            assert!(
                validate_tenant_name(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn traversal_cannot_escape_the_tenants_dir() {
        with_home(|_| {
            assert!(tenant_db_path("../../etc/passwd").is_err());
            assert!(legacy_user_db_path("../../etc/passwd").is_err());
        });
    }

    #[test]
    fn user_db_path_refuses_an_unadopted_legacy_database() {
        with_home(|home| {
            // Nothing anywhere: the tenant path is returned as-is, so a
            // fresh install just creates it.
            assert_eq!(
                user_db_path("collection").unwrap(),
                home.join("tenants").join("collection.sqlite")
            );

            // A pre-tenants database beside the catalog must NOT be
            // silently shadowed by an empty new one.
            std::fs::write(home.join("collection.sqlite"), b"").unwrap();
            let err = user_db_path("collection").unwrap_err().to_string();
            assert!(
                err.contains("pkdump tenant adopt"),
                "unhelpful error: {err}"
            );

            // Once adopted, the guard goes quiet even though the legacy
            // file's leftovers may linger.
            std::fs::create_dir_all(home.join("tenants")).unwrap();
            std::fs::write(home.join("tenants").join("collection.sqlite"), b"").unwrap();
            assert!(user_db_path("collection").is_ok());
        });
    }
}
