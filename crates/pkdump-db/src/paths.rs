//! Filesystem paths for PokeDumpster's databases.
//!
//! Layout — one shared catalog, one collection database per tenant:
//!
//! ```text
//! $PKDUMP_HOME/                     # default ~/.pkdump
//!   shared.sqlite                   # the catalog: one copy, ATTACHed by every tenant
//!   registry.sqlite                 # handle → database_id (see [`crate::registry`])
//!   tenants/
//!     collection.sqlite             # tenant `collection` (the original single user)
//!     <database_id>.sqlite          # one file per registered user, named by ULID
//! ```
//!
//! The path functions split by what they are handed, and that difference is
//! the whole point of `pd-fci1`:
//!
//! * [`tenant_db_path`] takes a *handle* — a name a person chose. It is the
//!   pre-registry layout, and now only the legacy migration ([`crate::tenants::adopt`])
//!   has any business with it.
//! * [`tenant_db_file`] and [`tenant_db_path_for_id`] take a *`database_id`*
//!   — an opaque ULID the registry minted. Nothing outside this process ever
//!   chose one, so no caller-supplied string reaches a path constructor.
//!   They differ only in where the directory comes from: the caller's, or
//!   the current `$PKDUMP_HOME`.
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

/// Path to the user registry database — the handle → `database_id` map.
///
/// Beside the catalog at the data root, deliberately NOT under `tenants/`:
/// that directory means "one file per tenant" exactly, which is what makes
/// the Litestream glob a correct description of the irreplaceable set.
/// The registry is irreplaceable too, but it is not a tenant, so it is
/// replicated as itself rather than by being smuggled into the glob.
pub fn registry_db_path() -> Result<PathBuf> {
    Ok(pkdump_home()?.join("registry.sqlite"))
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

/// Path to a tenant's collection database, named by *handle*. Validates the
/// name; does not touch the filesystem.
///
/// This is the pre-registry addressing. A registered user's database is named
/// by their `database_id` — see [`tenant_db_path_for_id`] — so the only
/// remaining callers are the legacy migration ([`crate::tenants::adopt`] /
/// [`crate::tenants::revert`]) and the single-tenant [`user_db_path`], neither
/// of which takes its name from a request.
pub fn tenant_db_path(name: &str) -> Result<PathBuf> {
    validate_tenant_name(name)?;
    Ok(tenants_dir()?.join(format!("{name}.sqlite")))
}

/// A canonical ULID is 26 characters of Crockford base32.
const DATABASE_ID_LEN: usize = 26;

/// Crockford base32, the alphabet a ULID renders in: the digits and the
/// uppercase letters minus `I`, `L`, `O` and `U`.
const CROCKFORD: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Reject anything that is not a canonical ULID as the registry mints them.
///
/// This is narrower than [`validate_tenant_name`] and it is a different
/// question. A tenant name is a *claim* — it arrives from outside and is
/// checked for what it must not contain. A `database_id` is *issued*:
/// [`crate::registry::insert`] is the only thing that writes one and it is
/// read back out of a `UNIQUE` column, so the check here is not "is this
/// safe" but "is this one of ours". Anything else — a handle, a header, a
/// hand-edited registry row — is not, and never becomes a path.
///
/// Checked rather than trusted because this is the one string that *does*
/// become a filename, and the epic exists because a string that became a
/// filename had only a regex between it and the filesystem. The alphabet
/// contains no `.`, `/` or `\`, so a value that passes cannot name anything
/// but a sibling inside `tenants/`.
///
/// Checked explicitly rather than by parsing with `Ulid::from_string`, which
/// accepts lowercase — and `01j…` and `01J…` are one file on a
/// case-insensitive filesystem but two S3 prefixes, the exact disagreement
/// [`validate_tenant_name`] refuses handles to prevent.
pub fn validate_database_id(id: &str) -> Result<()> {
    if id.len() != DATABASE_ID_LEN {
        return Err(DbError::Env(format!(
            "database id {id:?} is not {DATABASE_ID_LEN} characters"
        )));
    }
    if let Some(bad) = id.chars().find(|c| !CROCKFORD.contains(&(*c as u8))) {
        return Err(DbError::Env(format!(
            "database id {id:?} contains {bad:?}; expected Crockford base32"
        )));
    }
    Ok(())
}

/// A tenant directory + an issued `database_id` → that user's collection
/// database.
///
/// The **only** way a tenant database path is built at request time, and it
/// takes a `database_id` rather than a name for that reason: a handle read
/// off an unauthenticated header is a lookup key in [`crate::registry`],
/// never a path component. See `pd-rqgv`.
///
/// Takes the directory rather than deriving it so the server can hold the
/// one it resolved at startup; [`tenant_db_path_for_id`] is the
/// `$PKDUMP_HOME`-derived spelling.
pub fn tenant_db_file(dir: &std::path::Path, database_id: &str) -> Result<PathBuf> {
    validate_database_id(database_id)?;
    Ok(dir.join(format!("{database_id}.sqlite")))
}

/// Path to the collection database named by an opaque `database_id`, under
/// the [`tenants_dir`] of the current `$PKDUMP_HOME`.
///
/// The registry resolves a handle to one of these; this turns it into a file.
/// Validates the id; does not touch the filesystem.
pub fn tenant_db_path_for_id(database_id: &str) -> Result<PathBuf> {
    tenant_db_file(&tenants_dir()?, database_id)
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
    fn only_an_issued_database_id_becomes_a_path() {
        let dir = std::path::Path::new("/data/tenants");
        let id = crate::registry::mint_database_id();
        assert_eq!(
            tenant_db_file(dir, &id).unwrap(),
            dir.join(format!("{id}.sqlite"))
        );

        // Nothing a caller could send is one. A handle is not — not even a
        // perfectly ordinary one, which is the point: there is no string a
        // request can carry that this function will turn into a filename.
        for not_an_id in [
            "alice",
            "collection",
            "../../etc/passwd",
            "../shared",
            "a/b",
            "alice\0",
            "",
            &id.to_lowercase(),                      // ULIDs are uppercase
            &id[..25],                               // too short
            &format!("{id}X"),                       // too long
            &format!("{}I{}", &id[..13], &id[14..]), // outside Crockford
            // The right length, and still not an id: a separator does not
            // stop being a separator by arriving in a 26-character string.
            &format!("{}/{}", &id[..13], &id[14..]),
        ] {
            assert!(
                validate_database_id(not_an_id).is_err(),
                "{not_an_id:?} was accepted as a database id"
            );
            assert!(tenant_db_file(dir, not_an_id).is_err(), "{not_an_id:?}");
        }
    }

    #[test]
    fn traversal_cannot_escape_the_tenants_dir() {
        with_home(|_| {
            assert!(tenant_db_path("../../etc/passwd").is_err());
            assert!(legacy_user_db_path("../../etc/passwd").is_err());
            assert!(tenant_db_path_for_id("../../etc/passwd").is_err());
        });
    }

    #[test]
    fn a_database_id_names_a_file_under_tenants() {
        with_home(|home| {
            let id = ulid::Ulid::generate().to_string();
            assert_eq!(
                tenant_db_path_for_id(&id).unwrap(),
                home.join("tenants").join(format!("{id}.sqlite"))
            );
        });
    }

    #[test]
    fn database_ids_are_validated() {
        let good = ulid::Ulid::generate().to_string();
        assert_eq!(good.len(), DATABASE_ID_LEN);
        assert!(validate_database_id(&good).is_ok());

        // Built rather than typed out: a hand-counted 26-character literal
        // is how a length test ends up asserting nothing.
        let pad = |s: &str| format!("{s}{}", "0".repeat(DATABASE_ID_LEN - s.len()));
        for bad in [
            String::new(),
            "0".repeat(DATABASE_ID_LEN - 1),
            "0".repeat(DATABASE_ID_LEN + 1),
            good.to_lowercase(), // one file, two S3 prefixes
            pad("01I"),          // I is not in Crockford base32
            pad("01U"),          // nor U
            pad("../../etc/passwd"),
            pad("01J."), // a dot would extend the suffix
            pad("01J/"), // a separator would leave the directory
            pad("alice"),
        ] {
            assert!(validate_database_id(&bad).is_err(), "{bad:?}");
        }
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
