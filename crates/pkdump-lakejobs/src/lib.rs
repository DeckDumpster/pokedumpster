//! The lake's operational jobs, ported from `lake/src/pkdump_lake/` onto
//! `dumpster-kit-iceberg` (sp-4k7mxi.6).
//!
//! Three jobs, each the Python module's twin: [`freshness`] (`pkdump-lake-prices-age`),
//! [`nessie_backup`] (`pkdump-lake-nessie-backup`) and [`roundtrip`]
//! (`pkdump-lake-roundtrip`). Exit statuses, messages, the backup layout and the
//! manifest bytes are the Python's, because both clients run against one bucket
//! and one set of units until the timers switch; `tests/parity.rs` holds the pair
//! together against output recorded from the Python.
//!
//! Nothing here reads the clock: the scheduler names the instant (`--as-of`,
//! `--now`, `--backup-id`).

pub mod freshness;
pub mod nessie_backup;
pub mod roundtrip;
