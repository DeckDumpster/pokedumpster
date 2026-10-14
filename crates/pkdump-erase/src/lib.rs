//! PokeDumpster's deletion path: `dumpster-kit-erase`, bound to this site's key custody.
//!
//! Tombstone the tenant's key, drop their partition, and then prove the result by attempting
//! every read path and requiring each to fail — all of it the kit's, once. What is
//! PokeDumpster's is which master key the proof reads, and that is a name
//! ([`pkdump_site::key_env`]) rather than a behaviour, so these two functions only supply it.
//!
//! There is deliberately no timer: a deletion is an act somebody decides to perform on one
//! named account (`deploy/DELETION.md`).

use dumpster_kit_lake::{ObjectPurge, TenantZoneConfig};
use rusqlite::Connection;

pub use dumpster_kit_erase::{
    Check, Deletion, Dropped, EraseError, Proof, Result, StrayCopy, Sweep, Verdict, sweep,
};

/// The proof's types, at the path this crate has always spelled them.
pub mod verify {
    pub use dumpster_kit_erase::verify::*;
}

/// Delete `database_id` from the tenant zone and prove it. See [`dumpster_kit_erase::delete`].
pub fn delete(
    zone: &dyn ObjectPurge,
    config: &TenantZoneConfig,
    registry: &Connection,
    database_id: &str,
    reason: Option<&str>,
    stray: Option<&StrayCopy>,
) -> Result<Deletion> {
    dumpster_kit_erase::delete(
        &pkdump_site::key_env(),
        zone,
        config,
        registry,
        database_id,
        reason,
        stray,
    )
}

/// Attempt every read path to `database_id`'s holdings. See [`dumpster_kit_erase::verify()`].
pub fn verify(
    zone: &dyn ObjectPurge,
    config: &TenantZoneConfig,
    registry: &Connection,
    database_id: &str,
    stray: Option<&StrayCopy>,
) -> Result<Verdict> {
    dumpster_kit_erase::verify(
        &pkdump_site::key_env(),
        zone,
        config,
        registry,
        database_id,
        stray,
    )
}
