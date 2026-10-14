//! Answering an upstream request out of `raw/`.
//!
//! A raw partition is a **URL → bytes** map and always was, and replay is a lookup, not a
//! reimplementation. That lookup, and the rule that a miss is **fatal** — with a different
//! sentence for a URL the partition recorded as FAILED (pd-llbq), so a partial night replays
//! to the catalog the online refresh built — is `dumpster_kit_lakehouse::replay::RawReplay`,
//! once.
//!
//! What this file adds is the one thing the kit cannot know: the seam this site's ingest
//! reads a replay through, `pkdump_ingest::landing::ReplaySource`. [`SiteReplay`] is the
//! adapter, and its whole job is to turn the kit's error into the ingest error the run stops
//! with. There is deliberately no path from here to the network: a miss is the end of the
//! run, not a fallback.
//!
//! The one phase that still fetches on the offline path is set-symbol normalisation, and it
//! does not come through here at all — `symbols::normalize_all_symbols` builds its own client,
//! because images are deliberately outside `raw/`. See `pkdump-derive`'s crate docs, and
//! `row_identical.rs::a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it`
//! for the gate that holds that apart from this rule.

use dumpster_kit_lake::RawZone;
use dumpster_kit_lakehouse::replay::{RawReplay, ReplayError};
use pkdump_ingest::landing::ReplaySource;
use pkdump_ingest::{IngestError, Result};

use crate::partition::Chosen;

/// The kit's replay, presented to this site's ingest.
pub struct SiteReplay(RawReplay);

impl SiteReplay {
    /// Index `chosen`'s parts by URL. See [`RawReplay::new`].
    pub fn new(zone: RawZone, chosen: &[Chosen]) -> anyhow::Result<Self> {
        Ok(Self(RawReplay::new(zone, chosen)?))
    }

    /// How many distinct URLs this partition can answer.
    pub fn urls(&self) -> usize {
        self.0.urls()
    }
}

/// The kit cannot name a site's command, so its refusal says "the site's data refresh"; the
/// operator reading this one is told what to type. `row_identical.rs` asserts the sentence,
/// so a kit that rewords it fails there rather than printing something unactionable.
fn ingest_error(e: ReplayError) -> IngestError {
    match e {
        ReplayError::Miss(text) => {
            IngestError::BadResponse(text.replace("the site's data refresh", "pkdump data refresh"))
        }
        ReplayError::Lake(e) => IngestError::Lake(e),
    }
}

impl ReplaySource for SiteReplay {
    fn body(&self, url: &str) -> Result<Option<Vec<u8>>> {
        self.0.body(url).map_err(ingest_error)
    }

    fn missing(&self, url: &str) -> IngestError {
        ingest_error(self.0.missing(url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumpster_kit_lake::{DirStore, PartFormat, RawLanding};

    const STARTED: &str = "2026-08-11T04:51:02+00:00";
    const DATE: &str = "2026-08-11";
    const URL: &str = "https://api.pokemontcg.io/v2/sets";
    const BODY: &[u8] = br#"{"data":[]}"#;

    /// One run landing `sets` whole, as a replay over this site's own dataset.
    fn replay(root: &std::path::Path, fail_second: bool) -> SiteReplay {
        let sink = RawLanding::new(Box::new(DirStore::new(root)), DATE, STARTED);
        sink.land(
            pkdump_site::POKEMON_TCG_IO,
            pkdump_site::SETS,
            URL,
            200,
            PartFormat::Json,
            BODY,
        )
        .unwrap();
        if fail_second {
            sink.record_failure(
                pkdump_site::POKEMON_TCG_IO,
                pkdump_site::SETS,
                "https://api.pokemontcg.io/v2/sets?page=2",
                Some(502),
                "http 502 after 4 attempts",
            )
            .unwrap();
        }
        sink.finalize(None).unwrap();
        let zone = RawZone::new(Box::new(DirStore::new(root)));
        let runs = zone
            .runs(pkdump_site::POKEMON_TCG_IO, pkdump_site::SETS, DATE)
            .unwrap();
        let chosen = vec![Chosen {
            source: pkdump_site::POKEMON_TCG_IO,
            dataset: pkdump_site::SETS,
            run: dumpster_kit_lake::select_run(&runs, "pokemontcgio/sets")
                .unwrap_or_else(|_| runs.last().expect("one run landed").clone()),
        }];
        SiteReplay::new(zone, &chosen).unwrap()
    }

    #[test]
    fn a_landed_url_replays_the_bytes_that_were_landed() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path(), false);
        assert_eq!(replay.urls(), 1);
        assert_eq!(replay.body(URL).unwrap().as_deref(), Some(BODY));
    }

    /// The adapter's whole job: a miss is the ingest error the run stops with, and both
    /// sentences survive it — "re-land the date" for a gap, and the opposite for a night
    /// upstream was down.
    #[test]
    fn a_miss_and_a_recorded_failure_reach_the_run_as_different_sentences() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path(), true);

        assert!(replay.body("https://elsewhere/").unwrap().is_none());
        let gap = replay.missing("https://elsewhere/").to_string();
        assert!(gap.contains("raw/ has no record"), "{gap}");
        assert!(gap.contains("pkdump data refresh"), "{gap}");

        let failed = replay
            .missing("https://api.pokemontcg.io/v2/sets?page=2")
            .to_string();
        assert!(failed.contains("FAILED"), "{failed}");
        assert!(failed.contains("http 502"), "{failed}");
        assert!(failed.contains("Re-landing the date cannot"), "{failed}");
    }
}
