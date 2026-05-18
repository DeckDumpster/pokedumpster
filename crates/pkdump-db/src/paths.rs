//! Filesystem paths for PokeDumpster's databases.
//!
//! Layout (PLAN.md §3.1): a single `shared.sqlite` catalog plus one
//! `<user>.sqlite` per user, all under the PokeDumpster home directory.

use std::path::PathBuf;

use crate::error::{DbError, Result};

const DEFAULT_USER: &str = "collection";

/// The PokeDumpster data directory: `$PKDUMP_HOME` if set, else `$HOME/.pkdump`.
pub fn pkdump_home() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("PKDUMP_HOME") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")
        .map_err(|_| DbError::Env("neither PKDUMP_HOME nor HOME is set".into()))?;
    Ok(PathBuf::from(home).join(".pkdump"))
}

/// Path to the shared catalog database.
pub fn shared_db_path() -> Result<PathBuf> {
    Ok(pkdump_home()?.join("shared.sqlite"))
}

/// The active user: `$PKDUMP_USER` if set, else `collection`.
pub fn current_user() -> String {
    std::env::var("PKDUMP_USER").unwrap_or_else(|_| DEFAULT_USER.to_string())
}

/// Path to a given user's collection database.
pub fn user_db_path(user: &str) -> Result<PathBuf> {
    Ok(pkdump_home()?.join(format!("{user}.sqlite")))
}
