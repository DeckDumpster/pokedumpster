//! The one place `pkdump-db` reads the wall clock.
//!
//! Every timestamp column this crate stamps — `batches.created_at`,
//! `collection.acquired_at`, `binders`/`decks`, the wishlist, the sealed
//! shelf, the status log — comes from [`now_rfc3339`] and from nowhere else.
//! That is what makes the deterministic UI fixture possible at all: a
//! `chrono::Utc::now()` in a repository function is a value no rebuild can
//! reproduce, and `pkdump seed-fixture` writes its rows THROUGH these
//! functions on purpose, so the app-layer validation runs.
//!
//! [`clock::tests::the_wall_clock_is_read_in_one_place`] is the ratchet: a
//! repository function added tomorrow that reaches for `Utc::now()` directly
//! fails in a second, rather than silently un-pinning the fixture and
//! repainting four visual baselines the next time anybody regenerates it
//! (pd-nzlj).
//!
//! # Pinning
//!
//! [`pin`] replaces the clock with a fixed timeline that ADVANCES one step
//! per read. Advancing rather than frozen is the whole design: the fixture's
//! `/recent` and `/batches` routes `ORDER BY` these stamps, so a single
//! constant would make their order a tie broken by rowid — stable pixels,
//! and a route that has stopped demonstrating the ordering it exists to show.
//! A stepping clock reproduces the seeding sequence exactly, on a timeline
//! that is the same every time.
//!
//! The pin is **thread-local and never global**. A process-wide pin would
//! leak out of whichever test set it into every test running beside it, and
//! a clock that is wrong in one test out of a suite is the hardest kind of
//! flake to find. It also means production cannot be pinned from another
//! thread by accident; that it is not pinned from THIS one is
//! [`crates/pkdump-db/tests/pinned_clock_callers.rs`].

use std::cell::Cell;

use chrono::{DateTime, TimeDelta, Utc};

thread_local! {
    /// The pinned timeline, if this thread has one. `None` is the wall clock.
    static PINNED: Cell<Option<(DateTime<Utc>, TimeDelta)>> = const { Cell::new(None) };
}

/// The current instant, in the RFC-3339 spelling every timestamp column in
/// this crate uses.
pub fn now_rfc3339() -> String {
    now().to_rfc3339()
}

/// The current instant. Reads the wall clock unless this thread is pinned,
/// in which case it returns the next instant on the pinned timeline and
/// advances it.
pub fn now() -> DateTime<Utc> {
    PINNED.with(|p| match p.get() {
        None => Utc::now(),
        Some((next, step)) => {
            p.set(Some((next + step, step)));
            next
        }
    })
}

/// Pin this thread's clock to `start`, advancing `step` per read.
///
/// For building a reproducible fixture, and for nothing else — see
/// `pkdump-cli`'s `fixture.rs`, which is the only caller and is held to
/// that by `tests/pinned_clock_callers.rs`.
pub fn pin(start: DateTime<Utc>, step: TimeDelta) {
    PINNED.with(|p| p.set(Some((start, step))));
}

/// Give this thread the wall clock back.
pub fn unpin() {
    PINNED.with(|p| p.set(None));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_pinned_clock_advances_one_step_per_read() {
        pin(at("2024-01-15T09:00:00Z"), TimeDelta::minutes(1));
        assert_eq!(now_rfc3339(), "2024-01-15T09:00:00+00:00");
        assert_eq!(now_rfc3339(), "2024-01-15T09:01:00+00:00");
        assert_eq!(now_rfc3339(), "2024-01-15T09:02:00+00:00");
        unpin();
    }

    /// Two runs of the same sequence of reads produce the same timestamps.
    /// This is the property `seed-fixture` rests on, stated on its own.
    #[test]
    fn two_pinned_runs_read_the_same_instants() {
        let run = || {
            pin(at("2024-01-15T09:00:00Z"), TimeDelta::minutes(1));
            let out: Vec<String> = (0..8).map(|_| now_rfc3339()).collect();
            unpin();
            out
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn unpinning_gives_the_wall_clock_back() {
        pin(at("2024-01-15T09:00:00Z"), TimeDelta::minutes(1));
        unpin();
        assert!(
            (Utc::now() - now()).num_seconds().abs() < 60,
            "an unpinned clock must read the wall clock"
        );
    }

    /// A thread that never pinned is never pinned by one that did — the
    /// property that makes this safe to call from a test in a suite.
    #[test]
    fn a_pin_does_not_leak_into_another_thread() {
        pin(at("2024-01-15T09:00:00Z"), TimeDelta::minutes(1));
        let elsewhere = std::thread::spawn(now).join().unwrap();
        assert!(
            (Utc::now() - elsewhere).num_seconds().abs() < 60,
            "another thread saw the pinned timeline: {elsewhere}"
        );
        unpin();
    }

    /// Every wall-clock read in this crate goes through this module.
    ///
    /// The fixture is seeded through the repository functions, so a
    /// `Utc::now()` anywhere else in `pkdump-db` puts the build's own minute
    /// into a committed binary artefact — which is exactly how
    /// `tests/ui/fixtures/collection.sqlite` came to encode the afternoon it
    /// was generated, and how four visual baselines came to be re-recorded
    /// every time anybody regenerated it.
    ///
    /// Stated over the TREE rather than over the files that were wrong on the
    /// day: the failure mode is a repository function nobody has written yet.
    #[test]
    fn the_wall_clock_is_read_in_one_place() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();
        let mut scanned = 0;
        for entry in std::fs::read_dir(&src).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if name == "clock.rs" {
                continue;
            }
            scanned += 1;
            let body = std::fs::read_to_string(&path).unwrap();
            for (i, line) in body.lines().enumerate() {
                if line.contains("Utc::now()") || line.contains("Local::now()") {
                    offenders.push(format!("{name}:{}: {}", i + 1, line.trim()));
                }
            }
        }
        // Not vacuous: a scan that found no files would pass whatever the
        // crate does.
        assert!(scanned > 20, "scanned only {scanned} files in {src:?}");
        assert!(
            offenders.is_empty(),
            "pkdump-db reads the wall clock outside clock.rs, so a fixture \
             built through these functions cannot be reproduced. Use \
             `crate::clock::now_rfc3339()`:\n  {}",
            offenders.join("\n  "),
        );
    }
}
