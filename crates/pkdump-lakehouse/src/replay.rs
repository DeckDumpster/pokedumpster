//! Answering an upstream request out of `raw/`.
//!
//! A raw partition is a **URL → bytes** map and always was: the manifest
//! records the URL of every part beside its key and digest, precisely so the
//! landing zone can be read back without a parser. So replay is a lookup, not
//! a reimplementation — [`RawReplay`] resolves the URL a client was about to
//! request and hands back the body that URL returned when it was landed. The
//! derivation above it does not know, and must not know, which happened:
//! comparing two runs of one derivation is what makes "row-identical" a claim
//! about *provenance*. Comparing two implementations would only ever be a
//! claim about the second one.
//!
//! ## The fallback, and why it is loud
//!
//! A URL the partition has no record of means **raw coverage has regressed** —
//! the derivation grew an input the landing zone does not capture, or an
//! upstream's origin moved. Item 2 of the epic ships with a fallback for that
//! case, deliberately temporary (item 4 removes it, as its own reviewable
//! change, once row-identical is proven). While it exists the one thing it may
//! not do is succeed quietly:
//!
//! - every miss prints a `!! raw coverage has regressed` line naming the URL,
//! - the run ends with a summary line and a non-empty [`RawReplay::misses`],
//!   so a caller can fail a gate on it rather than reading logs,
//! - and `--no-upstream-fallback` turns the first miss into a refusal, which
//!   is what item 4 will make unconditional.
//!
//! A quiet fallback would make the landing zone decorative: every gate would
//! pass, every row would look right, and we would find out on the day an
//! upstream is down — the day the lake was bought for.

use std::collections::HashMap;
use std::sync::Mutex;

use pkdump_ingest::landing::ReplaySource;
use pkdump_ingest::{IngestError, Result};
use pkdump_lake::{PartRecord, RawZone};

use crate::partition::Chosen;

/// A URL-keyed view of one date's chosen runs.
pub struct RawReplay {
    zone: RawZone,
    /// URL → the part that URL's response landed as. Built once, at
    /// construction, from the manifests of the runs [`crate::partition::choose`]
    /// selected; the payload itself is read (and verified against the
    /// manifest's SHA-256) only when it is asked for.
    index: HashMap<String, PartRecord>,
    /// Whether a miss may reach the real upstream. Item 4 deletes this field
    /// and the branch it guards.
    fallback: bool,
    /// Every URL that missed, in request order.
    misses: Mutex<Vec<String>>,
}

impl RawReplay {
    /// Index `chosen`'s parts by URL.
    ///
    /// Two parts claiming one URL is a corrupt partition rather than a
    /// preference to resolve: within a single run a URL is fetched once, and
    /// [`choose`](crate::partition::choose) picks exactly one run per dataset,
    /// so a duplicate means the manifest disagrees with itself.
    pub fn new(zone: RawZone, chosen: &[Chosen], fallback: bool) -> anyhow::Result<Self> {
        let mut index: HashMap<String, PartRecord> = HashMap::new();
        for c in chosen {
            for part in &c.run.manifest.parts {
                if let Some(prior) = index.insert(part.url.clone(), part.clone())
                    && prior.key != part.key
                {
                    anyhow::bail!(
                        "run={} records two different parts for one URL: {} is both {} and {}.\n\
                         A URL is fetched once per run, so this manifest disagrees with itself \
                         and nothing here can decide which body that URL actually returned.",
                        c.run.run_id,
                        part.url,
                        prior.key,
                        part.key
                    );
                }
            }
        }
        Ok(Self {
            zone,
            index,
            fallback,
            misses: Mutex::new(Vec::new()),
        })
    }

    /// How many distinct URLs this partition can answer.
    pub fn urls(&self) -> usize {
        self.index.len()
    }

    /// Every URL that was not in `raw/`, in request order.
    ///
    /// Empty is the only good answer. A caller that finds this non-empty has
    /// evidence that the landing zone no longer covers the derivation's
    /// inputs, whether or not the fallback then succeeded in fetching them.
    pub fn misses(&self) -> Vec<String> {
        self.misses.lock().expect("misses lock").clone()
    }
}

impl ReplaySource for RawReplay {
    fn body(&self, url: &str) -> Result<Option<Vec<u8>>> {
        match self.index.get(url) {
            // `payload` verifies the bytes against the length and SHA-256 the
            // manifest recorded. A lake that hands back something other than
            // what it says it stored is worse than an empty one, because
            // everything downstream would believe it.
            Some(part) => Ok(Some(self.zone.payload(part)?)),
            None => Ok(None),
        }
    }

    fn missing(&self, url: &str) -> Result<()> {
        self.misses
            .lock()
            .expect("misses lock")
            .push(url.to_string());

        if !self.fallback {
            return Err(IngestError::BadResponse(format!(
                "raw/ has no record of {url}, and the upstream fallback is off.\n\
                 The landing zone no longer covers this derivation's inputs: either an \
                 endpoint was added without landing it, or the upstream's origin moved. \
                 Re-land the date (pkdump data refresh --land-raw) and derive again."
            )));
        }

        eprintln!(
            "!! raw coverage has REGRESSED: {url} is not in raw/ for this partition.\n\
             !! Falling back to the live upstream for it — the temporary fallback (epic item 2, \
             removed in item 4) is what is keeping this run alive, and a derive that needs it is \
             NOT reproducible from the lake. Land the missing endpoint."
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkdump_lake::{Dataset, DirStore, PartFormat, RawLanding, Source};

    const STARTED: &str = "2026-08-11T04:51:02+00:00";
    const DATE: &str = "2026-08-11";
    const URL: &str = "https://tcgcsv.com/tcgplayer/3/groups";
    const BODY: &[u8] = br#"{"results":[{"groupId":1}]}"#;

    fn landed(root: &std::path::Path) -> RawZone {
        let sink = RawLanding::new(Box::new(DirStore::new(root)), DATE, STARTED);
        sink.land(
            Source::Tcgcsv,
            Dataset::Groups,
            URL,
            200,
            PartFormat::Json,
            BODY,
        )
        .unwrap();
        sink.finalize(None).unwrap();
        RawZone::new(Box::new(DirStore::new(root)))
    }

    /// The one run this fixture lands, as a plan. `partition::choose` is
    /// deliberately not used here — it refuses a date missing three of the
    /// four required datasets, which is its own test's business, not this
    /// module's.
    fn replay(root: &std::path::Path, fallback: bool) -> RawReplay {
        let zone = landed(root);
        let runs = zone.runs(Source::Tcgcsv, Dataset::Groups, DATE).unwrap();
        let chosen = vec![Chosen {
            source: Source::Tcgcsv,
            dataset: Dataset::Groups,
            run: pkdump_lake::select_run(&runs, "tcgcsv/groups").unwrap(),
        }];
        RawReplay::new(zone, &chosen, fallback).unwrap()
    }

    #[test]
    fn a_landed_url_replays_the_bytes_that_were_landed() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path(), true);
        assert_eq!(replay.urls(), 1);
        assert_eq!(replay.body(URL).unwrap().as_deref(), Some(BODY));
        assert!(replay.misses().is_empty());
    }

    /// A miss is recorded whether or not the fallback then rescues the run.
    /// "It worked" is not the question — "did raw cover it" is.
    #[test]
    fn a_miss_is_recorded_even_when_the_fallback_is_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path(), true);
        assert_eq!(
            replay
                .body("https://tcgcsv.com/tcgplayer/3/9/prices")
                .unwrap(),
            None
        );
        replay
            .missing("https://tcgcsv.com/tcgplayer/3/9/prices")
            .expect("the fallback lets the fetch proceed");
        assert_eq!(
            replay.misses(),
            vec!["https://tcgcsv.com/tcgplayer/3/9/prices"]
        );
    }

    /// With the fallback off — what item 4 makes unconditional — the first
    /// miss stops the run, and the refusal says what regressed.
    #[test]
    fn with_the_fallback_off_a_miss_refuses_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path(), false);
        let err = replay
            .missing("https://up/3/9/prices")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("raw/ has no record of https://up/3/9/prices"),
            "{err}"
        );
        assert!(err.contains("--land-raw"), "{err}");
        assert_eq!(replay.misses().len(), 1);
    }

    /// The manifest's digest is not decoration. A part whose bytes changed
    /// under it must stop the derive, not feed it a plausible catalog.
    #[test]
    fn a_tampered_payload_fails_the_replay() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path(), true);
        // The SAME LENGTH, different bytes — otherwise the length check fires
        // first and the digest is never consulted. A corruption that changes
        // a value without changing a size is the one only the hash can catch.
        let key = &replay.index[URL].key;
        std::fs::write(
            tmp.path().join(key),
            zstd::encode_all(&br#"{"results":[{"groupId":9}]}"#[..], 1).unwrap(),
        )
        .unwrap();

        let err = replay.body(URL).unwrap_err().to_string();
        assert!(err.contains("sha256"), "{err}");
    }

    /// Two parts for one URL is a manifest that disagrees with itself. There
    /// is no right answer to pick, so there is no picking.
    #[test]
    fn one_url_cannot_have_two_bodies() {
        let tmp = tempfile::tempdir().unwrap();
        let zone = landed(tmp.path());
        let runs = zone.runs(Source::Tcgcsv, Dataset::Groups, DATE).unwrap();
        let mut run = pkdump_lake::select_run(&runs, "tcgcsv/groups").unwrap();
        let mut twin = run.manifest.parts[0].clone();
        twin.key = format!("{}-twin", twin.key);
        run.manifest.parts.push(twin);

        let err = match RawReplay::new(
            zone,
            &[Chosen {
                source: Source::Tcgcsv,
                dataset: Dataset::Groups,
                run,
            }],
            true,
        ) {
            Ok(_) => panic!("a manifest with two bodies for one URL must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("two different parts for one URL"), "{err}");
    }
}
