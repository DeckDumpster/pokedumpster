//! Filesystem paths for PokeDumpster's databases.
//!
//! The layout, the two identifier validators and the rule that a handle is never a path
//! component are the tenancy kernel's (`dumpster-kit-tenancy`, `paths`), shared with every other
//! site. What is PokeDumpster's is one thing: its name. [`app`] is the `PKDUMP` prefix, and every
//! function here is the kernel's, bound to it — `$PKDUMP_HOME` (default `~/.pkdump`) and
//! `$PKDUMP_USER` derive from it.
//!
//! ```text
//! $PKDUMP_HOME/
//!   shared.sqlite                   # the catalog: one copy, ATTACHed by every tenant
//!   registry.sqlite                 # handle → database_id (see [`crate::registry`])
//!   tenants/
//!     <database_id>.sqlite          # one file per registered user, named by ULID
//!     <handle>.sqlite               # ...or by handle, before `tenant migrate`
//! ```
//!
//! Provisioning lives in [`crate::tenants`]; this module is only paths.

use std::path::PathBuf;
use std::sync::OnceLock;

use dumpster_kit_tenancy::{self as kit, AppPrefix};

use crate::error::Result;

pub use dumpster_kit_tenancy::{HANDLE_RULE, TENANTS_DIR};

/// PokeDumpster's identity in the kernel.
pub fn app() -> &'static AppPrefix {
    static APP: OnceLock<AppPrefix> = OnceLock::new();
    APP.get_or_init(|| AppPrefix::new("PKDUMP").expect("PKDUMP is a valid app prefix"))
}

/// Reject anything that is not a handle (see [`HANDLE_RULE`]).
pub fn validate_tenant_name(name: &str) -> Result<()> {
    Ok(kit::validate_tenant_name(name)?)
}

/// Reject anything that is not a canonical ULID as the registry mints them.
pub fn validate_database_id(id: &str) -> Result<()> {
    Ok(kit::validate_database_id(id)?)
}

/// A tenants directory plus an issued `database_id` → that user's collection database. The only
/// way a tenant database path is built at request time.
pub fn tenant_db_file(dir: &std::path::Path, database_id: &str) -> Result<PathBuf> {
    Ok(kit::tenant_db_file(dir, database_id)?)
}

/// The PokeDumpster data directory: `$PKDUMP_HOME` if set, else `$HOME/.pkdump`.
pub fn pkdump_home() -> Result<PathBuf> {
    Ok(kit::home(app())?)
}

/// Path to the shared catalog database. One copy, shared by every tenant.
pub fn shared_db_path() -> Result<PathBuf> {
    Ok(kit::shared_db_path(app())?)
}

/// The directory holding every tenant's collection database.
pub fn tenants_dir() -> Result<PathBuf> {
    Ok(kit::tenants_dir(app())?)
}

/// Path to the user registry database — the handle → `database_id` map.
pub fn registry_db_path() -> Result<PathBuf> {
    Ok(kit::registry_db_path(app())?)
}

/// The active tenant: `$PKDUMP_USER` if set, else `collection`.
pub fn current_user() -> String {
    kit::current_user(app())
}

/// Path to a tenant's collection database, named by *handle* — the pre-registry addressing, for
/// the migrations that move a data directory between the two namings.
pub fn tenant_db_path(name: &str) -> Result<PathBuf> {
    Ok(kit::tenant_db_path(app(), name)?)
}

/// Path to the collection database named by an opaque `database_id`.
pub fn tenant_db_path_for_id(database_id: &str) -> Result<PathBuf> {
    Ok(kit::tenant_db_path_for_id(app(), database_id)?)
}

/// Where a tenant's collection database lived before the `tenants/` layout.
pub fn legacy_user_db_path(name: &str) -> Result<PathBuf> {
    Ok(kit::legacy_user_db_path(app(), name)?)
}

/// Run `f` against a throwaway `$PKDUMP_HOME`. The lock is the kernel's, shared by every test
/// that sets a home variable.
#[cfg(test)]
pub(crate) fn with_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    kit::paths::with_home(app(), f)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one thing PokeDumpster contributes: its prefix names the variables and the default
    /// directory. Everything else is held by the kernel's own tests and corpora.
    #[test]
    fn the_prefix_names_the_data_dir_and_the_tenant_variables() {
        assert_eq!(app().var("HOME"), "PKDUMP_HOME");
        assert_eq!(app().var("USER"), "PKDUMP_USER");
        assert_eq!(app().cli(), "pkdump");
        with_home(|home| {
            assert_eq!(pkdump_home().unwrap(), home);
            assert_eq!(
                tenant_db_path("alice").unwrap(),
                home.join("tenants").join("alice.sqlite")
            );
            assert_eq!(shared_db_path().unwrap(), home.join("shared.sqlite"));
            assert_eq!(registry_db_path().unwrap(), home.join("registry.sqlite"));
        });
    }

    /// Hostile names never become paths through PokeDumpster's wrappers either.
    #[test]
    fn traversal_cannot_escape_the_tenants_dir() {
        with_home(|_| {
            assert!(tenant_db_path("../../etc/passwd").is_err());
            assert!(legacy_user_db_path("../../etc/passwd").is_err());
            assert!(tenant_db_path_for_id("../../etc/passwd").is_err());
        });
    }
}
