//! The user registry: handle → `database_id`, identity bindings, and the retirement states.
//!
//! Wholly the tenancy kernel's (`dumpster_kit_tenancy::registry`, with `schema_registry.sql` and
//! its conformance corpora). PokeDumpster adds one function: [`open`], the registry under
//! `$PKDUMP_HOME`.

use rusqlite::Connection;

pub use dumpster_kit_tenancy::registry::*;

use crate::error::Result;

/// Open the registry under the current `$PKDUMP_HOME`, creating and migrating it.
pub fn open() -> Result<Connection> {
    Ok(dumpster_kit_tenancy::registry::open(crate::paths::app())?)
}
