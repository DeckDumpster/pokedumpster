//! Opening a landing zone.
//!
//! Two callers, and they want different answers to "there is no lake here":
//!
//! - [`open`] — `pkdump setup`, where landing is still **opt-in**. With the
//!   flag absent, `lake.env` is never read, no S3 client is built, and the
//!   fetch path behaves exactly as it did before the landing zone existed.
//!   Every offline gate and every container test stays offline.
//! - [`require`] — `pkdump data refresh`, which since pd-lunn *is* the landing
//!   run and has nothing else to do. There is no off for it.
//!
//! Either way, the destination must resolve or the command fails before it
//! fetches anything, naming the file the operator has to write. What must
//! never exist is a state where landing was asked for and quietly did nothing:
//! the whole value of the landing zone is that the bytes are there afterwards,
//! so "misconfigured" and "landed nothing" must not look alike from the
//! outside.
//!
//! This module is the *writing* half only. Nothing in `pkdump-cli` reads
//! `raw/` — that is `pkdump-lakehouse`'s job, on the other side of the
//! eventual machine split.

use std::sync::Arc;

use pkdump_derive::DeriveClock;
use pkdump_ingest::landing::Wire;
use pkdump_lake::RawLanding;

/// Enables landing without a command-line flag — how the containerised
/// nightly refresh turns it on, since its unit runs a fixed command line.
pub const ENV_ENABLE: &str = "PKDUMP_LAND_RAW";

/// Whether this invocation should land raw bytes.
pub fn enabled(flag: bool) -> bool {
    flag || matches!(
        std::env::var(ENV_ENABLE).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Open the configured landing zone, or `None` when landing is off.
///
/// The run's `clock` supplies both partition values that outlive it: its
/// day is the `ingest_date` every object lands under, and its instant is the
/// `started_at` every manifest records. That second one is what lets an
/// offline derive stamp the same `fetched_at` / `observed_at` values into the
/// same rows — see [`pkdump_derive::clock`].
///
/// Called before the first fetch, so a lake that was asked for and is not
/// configured stops the run immediately. Note what this does *not* do: it
/// builds an S3 client but does not probe the bucket, so a typo'd bucket
/// name or a credential the role cannot assume surfaces on the first PUT
/// rather than here. That PUT follows the first response by milliseconds,
/// which is early enough — and a preflight would demand a bucket permission
/// beyond the `PutObject` this actually needs.
pub fn open(flag: bool, clock: &DeriveClock) -> anyhow::Result<Option<Arc<RawLanding>>> {
    if !enabled(flag) {
        return Ok(None);
    }
    let ingest_date = clock.observed_date();
    let landing = pkdump_lake::open(ingest_date, clock.fetched_at())?;
    println!(
        "Landing raw upstream responses in {} (ingest_date={ingest_date})",
        landing.describe()
    );
    Ok(Some(Arc::new(landing)))
}

/// The landing zone, or a refusal — for the caller that has nothing to do
/// without one.
///
/// `pkdump data refresh` is that caller since pd-lunn: it no longer derives
/// anything, so a run with no landing zone would fetch every upstream, parse
/// none of it, write nothing anywhere and exit 0. That is not a degraded
/// refresh, it is an expensive no-op against somebody else's API — and it is
/// exactly the shape of failure this module exists to make impossible, one
/// level up. So landing is not a flag on that command any more; it is the
/// command.
///
/// The refusal names the file an operator has to write, the same way
/// [`open`]'s does, because "the lake is not configured" is a host-config
/// problem and never a code one.
pub fn require(clock: &DeriveClock) -> anyhow::Result<Arc<RawLanding>> {
    let ingest_date = clock.observed_date();
    let landing = pkdump_lake::open(ingest_date, clock.fetched_at()).map_err(|e| {
        anyhow::anyhow!(
            "{e:#}\n\n`pkdump data refresh` fetches the upstreams and LANDS them; the catalog \
             is built from that partition by `pkdump-lake-derive shared` (pd-lunn). Without a \
             landing zone there is nowhere for tonight's bytes to go and nothing that will ever \
             derive them, so the run refuses rather than fetching into a bin. Configure the \
             lake (deploy/LAKE.md §3) — or, on a box that has none, build the catalog with \
             `pkdump setup`."
        )
    })?;
    println!(
        "Landing raw upstream responses in {} (ingest_date={ingest_date})",
        landing.describe()
    );
    Ok(Arc::new(landing))
}

/// The wire a client fetches on: writing through to `landing` when there is
/// one, and otherwise exactly the client that existed before any of this.
///
/// There is no replay arm here, and there cannot be: constructing one needs a
/// [`ReplaySource`](pkdump_ingest::landing::ReplaySource) over `raw/`, and
/// nothing this binary links implements one.
pub fn wire(landing: Option<&Arc<RawLanding>>) -> Wire {
    match landing {
        Some(landing) => Wire::default().landing_in(Arc::clone(landing)),
        None => Wire::default(),
    }
}
