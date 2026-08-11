//! Turning `--land-raw` into a landing zone.
//!
//! Landing is **opt-in**, and the two states are deliberately far apart:
//!
//! - **off** — the flag is absent, `lake.env` is never read, no S3 client is
//!   built, and the fetch path behaves exactly as it did before the landing
//!   zone existed. Every offline gate and every container test stays offline.
//! - **on** — the destination must resolve or the command fails before it
//!   fetches anything, naming the file the operator has to write.
//!
//! What must never exist is a third state where landing was asked for and
//! quietly did nothing: the whole value of the landing zone is that the
//! bytes are there afterwards, so "misconfigured" and "landed nothing" must
//! not look alike from the outside.

use std::sync::Arc;

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
/// `ingest_date` is the `YYYY-MM-DD` partition every object of this
/// invocation lands under.
///
/// Called before the first fetch, so a lake that was asked for and is not
/// configured stops the run immediately. Note what this does *not* do: it
/// builds an S3 client but does not probe the bucket, so a typo'd bucket
/// name or a credential the role cannot assume surfaces on the first PUT
/// rather than here. That PUT follows the first response by milliseconds,
/// which is early enough — and a preflight would demand a bucket permission
/// beyond the `PutObject` this actually needs.
pub fn open(flag: bool, ingest_date: &str) -> anyhow::Result<Option<Arc<RawLanding>>> {
    if !enabled(flag) {
        return Ok(None);
    }
    let landing = pkdump_lake::open(ingest_date)?;
    println!(
        "Landing raw upstream responses in {} (ingest_date={ingest_date})",
        landing.describe()
    );
    Ok(Some(Arc::new(landing)))
}

/// Attach a landing zone to a client, when there is one.
///
/// The builder methods take `self` so that a client with no landing zone is
/// literally the client that existed before this feature — there is no
/// `Option` on the hot path to reason about.
pub fn with_landing<C>(
    client: C,
    landing: Option<&Arc<RawLanding>>,
    attach: impl FnOnce(C, Arc<RawLanding>) -> C,
) -> C {
    match landing {
        Some(landing) => attach(client, Arc::clone(landing)),
        None => client,
    }
}

/// Write the run's manifests and report what landed.
///
/// `error` is the acquisition phase's failure, if it had one; every manifest
/// then records that the run stopped early. A manifest that cannot be
/// written is an error in its own right — an unwritten manifest is
/// indistinguishable from a run that never got that far, which is the
/// ambiguity this file exists to prevent — but it must not mask the fetch
/// failure that is the more useful diagnosis.
pub fn finalize_landing(
    landing: &Arc<RawLanding>,
    error: Option<&anyhow::Error>,
) -> anyhow::Result<()> {
    let text = error.map(|e| format!("{e:#}"));
    let outcome = landing.finalize(text.as_deref());

    for manifest in landing.manifests() {
        println!(
            "  raw: {}/{} — {} part(s), {} byte(s), {}",
            manifest.source,
            manifest.dataset,
            manifest.parts.len(),
            manifest.total_bytes(),
            if manifest.complete {
                "complete".to_string()
            } else {
                format!("INCOMPLETE ({} failure(s))", manifest.failures.len())
            }
        );
    }

    match outcome {
        Ok(()) => Ok(()),
        // The acquisition error is the one worth propagating; this one still
        // has to be said out loud rather than dropped.
        Err(e) if error.is_some() => {
            eprintln!("WARN: could not write the raw landing manifests: {e}");
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}
