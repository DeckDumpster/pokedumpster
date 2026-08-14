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
//! ## A miss is fatal
//!
//! A URL the partition has no record of means **raw coverage has regressed** —
//! the derivation grew an input the landing zone does not capture, or an
//! upstream's origin moved. That is a refusal, full stop.
//!
//! Item 2 of the epic shipped with a temporary fallback for that case: reach
//! the live upstream, say so loudly, and let the run finish. It was there to
//! keep the offline derive usable while row-identity was still unproven, and
//! item 4 deleted it once pd-vves proved it against the real bucket. What is
//! left is the behaviour `--no-upstream-fallback` used to select, now the only
//! behaviour there is — and it is structural rather than a policy this module
//! chooses: [`ReplaySource::missing`] returns the error itself, so a replaying
//! wire has no path to the network for this module to decline to take.
//!
//! The reason it could not stay is that a fallback makes the landing zone
//! decorative. A derive that reaches upstream produces a correct catalog with a
//! LINEAGE that is not reproducible — every gate passes, every row looks right,
//! and we would find out on the day an upstream is down, which is the day the
//! lake was bought for. Loudness was a mitigation for that, not a fix: it
//! depends on somebody reading the log.
//!
//! The one phase that still fetches on the offline path is set-symbol
//! normalisation, and it does not come through here at all —
//! `symbols::normalize_all_symbols` has no [`Wire`](pkdump_ingest::landing::Wire)
//! in its signature and builds its own client, because images are deliberately
//! outside `raw/`. See `pkdump-derive`'s crate docs, and
//! `row_identical.rs::a_cold_derive_fetches_set_symbols_live_and_is_not_refused_for_it`
//! for the gate that holds that apart from this rule.

use std::collections::HashMap;

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
}

impl RawReplay {
    /// Index `chosen`'s parts by URL.
    ///
    /// Two parts claiming one URL is a corrupt partition rather than a
    /// preference to resolve: within a single run a URL is fetched once, and
    /// [`choose`](crate::partition::choose) picks exactly one run per dataset,
    /// so a duplicate means the manifest disagrees with itself.
    pub fn new(zone: RawZone, chosen: &[Chosen]) -> anyhow::Result<Self> {
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
        Ok(Self { zone, index })
    }

    /// How many distinct URLs this partition can answer.
    pub fn urls(&self) -> usize {
        self.index.len()
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

    fn missing(&self, url: &str) -> IngestError {
        IngestError::BadResponse(format!(
            "raw/ has no record of {url}.\n\
             The landing zone no longer covers this derivation's inputs: either an \
             endpoint was added without landing it, or the upstream's origin moved. \
             Re-land the date (pkdump data refresh --land-raw) and derive again."
        ))
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
    fn replay(root: &std::path::Path) -> RawReplay {
        let zone = landed(root);
        let runs = zone.runs(Source::Tcgcsv, Dataset::Groups, DATE).unwrap();
        let chosen = vec![Chosen {
            source: Source::Tcgcsv,
            dataset: Dataset::Groups,
            run: pkdump_lake::select_run(&runs, "tcgcsv/groups").unwrap(),
        }];
        RawReplay::new(zone, &chosen).unwrap()
    }

    #[test]
    fn a_landed_url_replays_the_bytes_that_were_landed() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path());
        assert_eq!(replay.urls(), 1);
        assert_eq!(replay.body(URL).unwrap().as_deref(), Some(BODY));
    }

    /// A URL the partition has no record of stops the run, and the refusal
    /// says what regressed and what to do about it. There is no second
    /// behaviour to select any more: the fallback that used to make this a
    /// warning is gone (item 4).
    #[test]
    fn a_miss_refuses_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path());
        assert_eq!(
            replay
                .body("https://tcgcsv.com/tcgplayer/3/9/prices")
                .unwrap(),
            None,
            "a URL outside the partition has no body to hand back"
        );
        let err = replay.missing("https://up/3/9/prices").to_string();
        assert!(
            err.contains("raw/ has no record of https://up/3/9/prices"),
            "{err}"
        );
        assert!(err.contains("--land-raw"), "{err}");
    }

    /// The manifest's digest is not decoration. A part whose bytes changed
    /// under it must stop the derive, not feed it a plausible catalog.
    #[test]
    fn a_tampered_payload_fails_the_replay() {
        let tmp = tempfile::tempdir().unwrap();
        let replay = replay(tmp.path());
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
        ) {
            Ok(_) => panic!("a manifest with two bodies for one URL must be refused"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("two different parts for one URL"), "{err}");
    }
}
