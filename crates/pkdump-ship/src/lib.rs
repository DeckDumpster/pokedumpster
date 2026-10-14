//! PokeDumpster's shipper: `dumpster-kit-ship`, bound to this site.
//!
//! The shipper itself — the part layout, the sealed envelope, the cursor and gap ledger, the
//! zone read-back — is the kit's, once. What is PokeDumpster's is the four things the kit
//! cannot know: which tables are holdings ([`HOLDINGS_SOURCES`], with the triggers on them in
//! `pkdump-db`), how a tenant database is opened (`pkdump_db::open_user`, whose schema gate
//! must apply here too), where the registry lives, and the names its configuration derives
//! from ([`pkdump_site`]).
//!
//! Nothing here re-implements a step. Every item is a binding: a table name, a conversion
//! between this site's registry row and the kit's, or a kit call with this site's arguments
//! filled in.
//!
//! The operator binary is `pkdump-ship` (`src/bin/pkdump-ship.rs`) and `deploy/ship.sh` is
//! what turns its four exit statuses into a journal line and a page.

use std::path::Path;

use dumpster_kit_keys::TenantKey;
use dumpster_kit_lake::{ObjectSource, ObjectStore, TenantZoneConfig};
use dumpster_kit_ship::run::ShipContext;
use rusqlite::Connection;

pub use dumpster_kit_ship::{
    Event, Gap, Outcome, Part, Report, ShipError, Status, TenantOutcome, ZoneHoldings, cipher,
    cursor, encode, outbox, plan, run, zone,
};

/// The staging table Phase 3 reads for loose cards. Created by [`materialize`] from
/// `collection`, and named here because two implementations spell it — the shipper's read-back
/// and the transform's SQL.
pub const HOLDINGS_TABLE: &str = "zone_holdings";

/// The same for sealed product, derived from `sealed_collection` (`pd-bbv7`).
pub const SEALED_HOLDINGS_TABLE: &str = "zone_sealed_holdings";

/// Where the rows in [`HOLDINGS_TABLE`] came from: which parts, how far through the outbox,
/// and when.
pub const HOLDINGS_RUN_TABLE: &str = dumpster_kit_ship::zone::HOLDINGS_RUN_TABLE;

/// The outbox source tables whose events become holdings, each with the staging table its
/// reduction lands in.
///
/// This is the reader's whole notion of what the zone carries, and it is held to
/// `pkdump_db::outbox::SOURCE_TABLES` by `every_outbox_source_has_a_staging_table`: a source
/// the outbox ships and this reader has no table for would be declined every night, silently
/// and forever, which is exactly how sealed stayed out of every valuation.
pub const HOLDINGS_SOURCES: &[(&str, &str)] = &[
    ("collection", HOLDINGS_TABLE),
    ("sealed_collection", SEALED_HOLDINGS_TABLE),
];

/// The kit's view of one of this site's tenants.
pub fn tenant(t: &pkdump_db::tenants::Tenant) -> dumpster_kit_ship::Tenant {
    dumpster_kit_ship::Tenant {
        user: kit_user(&t.user),
        path: t.path.clone(),
    }
}

/// This site's registry row as the kit's.
///
/// The two registries are one table with one schema (`schema_registry.sql`); only the Rust
/// type differs until the tenancy cutover makes them the same one.
pub fn kit_user(u: &pkdump_db::registry::User) -> dumpster_kit_tenancy::registry::User {
    dumpster_kit_tenancy::registry::User {
        database_id: u.database_id.clone(),
        handle: u.handle.clone(),
        created_at: u.created_at.clone(),
        state: match u.state {
            pkdump_db::registry::UserState::Active => {
                dumpster_kit_tenancy::registry::UserState::Active
            }
            pkdump_db::registry::UserState::Detached => {
                dumpster_kit_tenancy::registry::UserState::Detached
            }
        },
        retired_at: u.retired_at.clone(),
    }
}

/// Everything one shipping run needs that is not a tenant, with this site's answers.
pub fn with_context<T>(
    zone: &dyn ObjectStore,
    config: &TenantZoneConfig,
    registry: &Connection,
    f: impl FnOnce(&ShipContext<'_>) -> T,
) -> T {
    let keys = pkdump_site::key_env();
    let (crash_var, cli) = ShipContext::names(&pkdump_site::app());
    let open_tenant = |path: &Path| -> dumpster_kit_ship::Result<Connection> {
        pkdump_db::open_user(path).map_err(|e| ShipError::Tenancy(tenancy_error(e)))
    };
    f(&ShipContext {
        keys: &keys,
        zone,
        config,
        registry,
        open_tenant: &open_tenant,
        crash_var,
        cli,
    })
}

/// A `pkdump-db` failure as the kernel's, for the kit's `open_tenant` seam. A schema-version
/// refusal stays a schema-version refusal — it is the one error there that is never retryable.
fn tenancy_error(e: pkdump_db::DbError) -> dumpster_kit_tenancy::TenancyError {
    use dumpster_kit_tenancy::TenancyError;
    match e {
        pkdump_db::DbError::SchemaVersion(m) => TenancyError::SchemaVersion(m),
        pkdump_db::DbError::Sqlite(e) => TenancyError::Sqlite(e),
        other => TenancyError::Env(other.to_string()),
    }
}

/// Ship every tenant in `tenants`, one at a time. See [`dumpster_kit_ship::ship_all`].
pub fn ship_all(
    zone: &dyn ObjectStore,
    config: &TenantZoneConfig,
    registry: &Connection,
    tenants: &[pkdump_db::tenants::Tenant],
    max_rows: usize,
) -> Report {
    let kit: Vec<_> = tenants.iter().map(tenant).collect();
    with_context(zone, config, registry, |ctx| {
        dumpster_kit_ship::ship_all(ctx, &kit, max_rows)
    })
}

/// Ship one tenant. See [`dumpster_kit_ship::ship_one`].
pub fn ship_one(
    zone: &dyn ObjectStore,
    config: &TenantZoneConfig,
    registry: &Connection,
    tenant_: &pkdump_db::tenants::Tenant,
    max_rows: usize,
) -> TenantOutcome {
    with_context(zone, config, registry, |ctx| {
        dumpster_kit_ship::ship_one(ctx, &tenant(tenant_), max_rows)
    })
}

/// Read a tenant's holdings back out of the zone, for this site's sources.
pub fn read(
    source: &dyn ObjectSource,
    config: &TenantZoneConfig,
    key: &TenantKey,
    database_id: &str,
) -> dumpster_kit_ship::Result<ZoneHoldings> {
    dumpster_kit_ship::zone::read(source, config, key, database_id, HOLDINGS_SOURCES)
}

/// Stage what [`read`] found into this site's staging tables.
pub fn materialize(
    conn: &Connection,
    holdings: &ZoneHoldings,
    read_at: &str,
) -> dumpster_kit_ship::Result<usize> {
    dumpster_kit_ship::zone::materialize(conn, holdings, HOLDINGS_SOURCES, read_at)
}

/// The per-tenant key for `database_id`, under this site's key custody.
pub fn tenant_key(
    registry: &Connection,
    database_id: &str,
) -> dumpster_kit_keys::Result<TenantKey> {
    dumpster_kit_keys::tenant_key(&pkdump_site::key_env(), registry, database_id)
}
