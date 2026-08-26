//! Which raw runs a date's derive reads, and when it refuses to run at all.
//!
//! The derive is scheduled separately from the landing, deliberately: a
//! derive can then run against yesterday's raw on a night the fetch failed.
//! The trap on the other side of that feature is the one this module exists
//! to close —
//!
//! > yesterday's raw silently deriving today's `shared` and looking current.
//!
//! So there is no "newest available" anywhere in here. The job is told a date
//! and builds *that* date or stops. What this module decides is the narrower
//! question of which of a date's runs to read, and what makes a date
//! underivable:
//!
//! - **a required dataset that landed nothing** — the date was never fetched,
//!   or was fetched by something that is not a refresh. Refuse.
//! - **a required dataset with no complete run** — parts exist, but nothing in
//!   the landing zone can say whether they add up to a day (the writer never
//!   learns how many parts it was owed). A smaller catalog reads as *cards
//!   that do not exist*, so refuse rather than build one.
//! - **runs that disagree about the clock** — see [`clock_of`].
//!
//! ## The one incompleteness that is not a refusal (pd-llbq)
//!
//! The pokemontcg.io **tail** is exempt, and only it. On a night that upstream
//! is having — 5xx to ~45% of requests on 2026-08-11 — the tail spends its
//! retries and gives up, `pkdump data refresh` carries on and exits 2
//! (pd-nons), and the landing zone is left honest about it: `pokemontcgio/sets`
//! records the failure and reads `complete: false` while `tcgcsv/prices` reads
//! complete, because [`finalize`](pkdump_lake::RawLanding::finalize) computes
//! completeness per dataset.
//!
//! Refusing that partition outright made the two units answer one night's
//! weather in opposite ways: the online refresh said PARTIAL and kept the
//! prices, the offline derive said "no complete run" and paged. Which costs
//! nothing while the online refresh still builds the catalog inline — and
//! costs the night's **prices** at epic item 6, when this job is the only
//! builder. That is exactly the loss pd-nons exists to prevent, one side of
//! the split later.
//!
//! So the asymmetry is deliberate and it is narrow:
//!
//! | | an incomplete prefix means |
//! | --- | --- |
//! | `tcgcsv/*` | **refuse** — 200 groups of an unknown 450; the shortfall is unknowable |
//! | `pokemontcgio/sets`, `pokemontcgio/cards` | **PARTIAL** — the set list is as old as the last run that finished one, and tomorrow's is a superset |
//!
//! A partial partition still derives, and the derive still says so: the job
//! exits 2, `deploy/derive.sh` pushes a warning, and the provenance rows carry
//! `complete: false` for the datasets that were short. The replay does the
//! rest on its own — the URL the tail died on was never landed, so the tail
//! fails again at the same request, which is why a partial night replays to
//! the catalog the online refresh built rather than to a different one.
//!
//! Two datasets are optional, and both for the same reason: an ordinary
//! refresh does not necessarily fetch them. `pokemontcgio/cards` is fetched
//! only for sets the catalog does not already have, so a night with no new
//! set lands none; `pokemon-tcg-data/bulk` is `pkdump setup`'s bulk corpus and
//! a refresh never fetches it at all. Absent, they contribute no URLs and any
//! request for one becomes a replay miss — which is loud (see
//! [`crate::replay`]) rather than silent.

use std::collections::BTreeMap;

use pkdump_derive::DeriveClock;
use pkdump_lake::{Dataset, RawZone, Run, Source, select_run};

/// Whether a derive can proceed without a dataset's bytes at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Every refresh fetches this. Absent is a refusal.
    Required,
    /// A refresh may legitimately not have fetched this. Absent contributes
    /// no URLs and any request for one becomes a replay miss, which is loud.
    Optional,
}

/// What a dataset that landed but did not FINISH means for the derive.
///
/// Separate from [`Need`] because the two questions have different answers for
/// the same dataset: `pokemontcgio/cards` may be absent (an ordinary night with
/// no new set) *and* may be short (the night the tail died), and those are not
/// the same fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incomplete {
    /// A refusal. An incomplete run's parts are real bytes, but nothing in the
    /// landing zone can say whether they add up to a day — the writer never
    /// learns how many parts it was owed. A catalog that is quietly smaller
    /// reads as *cards that do not exist*.
    Refuse,
    /// A **partial** derivation, not a refusal — the pokemontcg.io tail, and
    /// only it. See the module docs: the shortfall is bounded (a stale set
    /// list, superseded by tomorrow's) where TCGCSV's is not, and refusing it
    /// would cost the night's prices once this job is the only builder.
    Partial,
}

/// Where a dataset's payloads come from, whether a derive needs them, and what
/// a short prefix of them means.
///
/// Exhaustive over [`Dataset`] with no wildcard arm, on purpose: landing a new
/// dataset stops this compiling until somebody decides whether a rebuild may
/// run without it. That decision is exactly the coverage question item 1 of
/// the epic made a test rather than a review, and this is where it is asked
/// for the *reading* side.
pub fn requirement(dataset: Dataset) -> (Source, Need, Incomplete) {
    match dataset {
        // THE half a night cannot lose. A price is a fact about one day and
        // there is no asking for it later, so a prefix that may be short of
        // an unknown number of groups is not something to derive from.
        Dataset::Groups => (Source::Tcgcsv, Need::Required, Incomplete::Refuse),
        Dataset::Products => (Source::Tcgcsv, Need::Required, Incomplete::Refuse),
        Dataset::Prices => (Source::Tcgcsv, Need::Required, Incomplete::Refuse),
        // The tail. Absent is still a refusal — a date no refresh reached the
        // tail of is a date nothing landed — but SHORT is a partial night
        // rather than an underivable one (pd-llbq).
        Dataset::Sets => (Source::PokemonTcgIo, Need::Required, Incomplete::Partial),
        // Fetched only for sets the catalog lacks — a night with no new set
        // lands none, and that is an ordinary night rather than a gap. Short
        // is the tail dying between two sets, which is the same partial night.
        Dataset::Cards => (Source::PokemonTcgIo, Need::Optional, Incomplete::Partial),
        // `pkdump setup`'s bulk corpus. A refresh never fetches it, and a
        // `setup` that fetched half of it derives half a catalog.
        Dataset::Bulk => (Source::PokemonTcgData, Need::Optional, Incomplete::Refuse),
    }
}

/// One dataset's chosen run for the date being derived.
#[derive(Debug, Clone)]
pub struct Chosen {
    /// Where the payloads came from.
    pub source: Source,
    /// Which endpoint's payloads.
    pub dataset: Dataset,
    /// The run selected — newest complete, per [`select_run`].
    pub run: Run,
}

impl Chosen {
    /// `source/dataset date`, the phrase every refusal names.
    pub fn what(&self, ingest_date: &str) -> String {
        format!("{}/{} {ingest_date}", self.source, self.dataset)
    }
}

/// The runs a date's derive will read, and whether the night was whole.
#[derive(Debug, Clone)]
pub struct Plan {
    /// One run per dataset, in [`Dataset::ALL`] order.
    pub chosen: Vec<Chosen>,
    /// One line per dataset that landed short — empty on a whole night.
    ///
    /// Only the pokemontcg.io tail can populate this; every other dataset's
    /// incompleteness is a refusal, so `choose` returns `Err` instead. See
    /// [`Incomplete`] and the module docs.
    pub partial: Vec<String>,
}

impl Plan {
    /// Whether this partition is a partial night.
    pub fn is_partial(&self) -> bool {
        !self.partial.is_empty()
    }
}

/// Choose one run per dataset for `ingest_date`, refusing a date that cannot
/// be derived.
///
/// Returns the chosen runs in [`Dataset::ALL`] order — deterministic, so the
/// job's output and its provenance rows do not depend on a listing's order.
///
/// A dataset the landing zone marked short is a refusal for everything except
/// the tail, where it is recorded in [`Plan::partial`] and derived from
/// anyway. The refusals still win over that: a TCGCSV dataset with no complete
/// run returns `Err` whatever the tail did, so "partial" can only ever mean
/// "the set list is old", never "some of the prices are missing".
pub fn choose(zone: &RawZone, ingest_date: &str) -> anyhow::Result<Plan> {
    let mut chosen = Vec::new();
    let mut partial = Vec::new();
    for &dataset in Dataset::ALL {
        let (source, need, incomplete) = requirement(dataset);
        let what = format!("{source}/{dataset} {ingest_date}");
        let runs = zone.runs(source, dataset, ingest_date)?;

        if runs.is_empty() && need == Need::Optional {
            println!("  {what}: nothing landed (optional)");
            continue;
        }
        // `select_run` is the shared rule — newest complete wins, no complete
        // run refuses — and it is deliberately the same function the Python
        // side documents, so two jobs cannot disagree about what a date IS.
        let run = match select_run(&runs, &what) {
            Ok(run) => {
                println!(
                    "  {what}: {} ({} part(s))",
                    run.run_id,
                    run.manifest.parts.len()
                );
                run
            }
            // The tail, and only the tail. `runs` is non-empty here (an empty
            // one is either the `Optional` skip above or `select_run`'s "no
            // runs landed", which stays a refusal for the tail too — a date
            // nothing landed is not a partial night, it is not a night), and
            // it is ordered oldest-first, so the last is the newest attempt.
            Err(_) if incomplete == Incomplete::Partial && !runs.is_empty() => {
                let run = runs.last().expect("runs is not empty").clone();
                println!("  {what}: {} — INCOMPLETE", run.describe());
                partial.push(format!("{what}: {}", run.describe()));
                run
            }
            Err(e) => return Err(e.into()),
        };
        chosen.push(Chosen {
            source,
            dataset,
            run,
        });
    }
    Ok(Plan { chosen, partial })
}

/// The clock every chosen run agrees on.
///
/// A derive reproduces **one fetch**. Its rows carry `fetched_at` and
/// `observed_at` values that came from the clock the fetching run read once,
/// and the manifest is where that instant was written down — so recovering it
/// is what makes "row-identical" mean identical rather than
/// identical-apart-from-the-timestamps.
///
/// Two runs of one date that disagree about it are two different fetches, and
/// rows stamped from a blend of them would be neither run's output. That is
/// refused, naming both, rather than resolved by a rule (newest? first?) that
/// would quietly pick one fetch's bytes and the other's clock. It happens when
/// a date was landed by two invocations — a `setup` and a `refresh` on the same
/// day, say — and the remedy is to derive a date one run landed.
///
/// A run whose manifest carries no clock at all is refused by
/// [`DeriveClock::from_manifest`], which says which partition and what to do.
pub fn clock_of(chosen: &[Chosen], ingest_date: &str) -> anyhow::Result<DeriveClock> {
    let mut by_instant: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for c in chosen {
        by_instant
            .entry(c.run.manifest.started_at.trim())
            .or_default()
            .push(format!("{} run={}", c.what(ingest_date), c.run.run_id));
    }

    if by_instant.len() > 1 {
        let detail = by_instant
            .iter()
            .map(|(instant, who)| {
                let instant = if instant.is_empty() {
                    "(none)"
                } else {
                    instant
                };
                format!("{instant}: {}", who.join(", "))
            })
            .collect::<Vec<_>>()
            .join("\n  ");
        anyhow::bail!(
            "{ingest_date}: the runs this derive would read were fetched at different \
             instants, so there is no single clock to stamp its rows with:\n  {detail}\n\
             A derive reproduces one fetch. Rows built from one run's bytes and another \
             run's clock are neither run's output, which is precisely the \"old data looks \
             new\" failure the ingest_date partition exists to prevent.\n\
             Derive a date that one run landed, or re-land this one \
             (pkdump data refresh) so every dataset comes from the same run."
        );
    }

    let (instant, who) = by_instant
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("{ingest_date}: no runs were chosen"))?;
    DeriveClock::from_manifest(instant, &who.join(", "))
}

/// The provenance rows for a completed derive — see
/// [`pkdump_db::raw_derivation`].
pub fn provenance(
    chosen: &[Chosen],
    ingest_date: &str,
    clock: &DeriveClock,
) -> Vec<pkdump_db::raw_derivation::RawDerivation> {
    chosen
        .iter()
        .map(|c| pkdump_db::raw_derivation::RawDerivation {
            ingest_date: ingest_date.to_string(),
            source: c.source.to_string(),
            dataset: c.dataset.to_string(),
            run_id: c.run.run_id.clone(),
            parts: c.run.manifest.parts.len() as i64,
            complete: c.run.manifest.complete,
            observed_at: clock.observed_date().to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkdump_lake::{DirStore, PartFormat, RawLanding};

    const STARTED: &str = "2026-08-11T04:51:02+00:00";
    const DATE: &str = "2026-08-11";

    /// Land the four required datasets through the real writer, so the reader
    /// under test is looking at bytes the landing zone actually produced.
    fn land(root: &std::path::Path, date: &str, started: &str, fail: bool) -> String {
        let sink = RawLanding::new(Box::new(DirStore::new(root)), date, started);
        for (source, dataset, url) in [
            (Source::Tcgcsv, Dataset::Groups, "https://up/3/groups"),
            (Source::Tcgcsv, Dataset::Products, "https://up/3/1/products"),
            (Source::Tcgcsv, Dataset::Prices, "https://up/3/1/prices"),
            (Source::PokemonTcgIo, Dataset::Sets, "https://up/sets"),
        ] {
            sink.land(source, dataset, url, 200, PartFormat::Json, b"{}")
                .unwrap();
        }
        sink.finalize(if fail { Some("http: 503") } else { None })
            .unwrap();
        sink.run_id().to_string()
    }

    /// The night `api.pokemontcg.io` is down: TCGCSV lands whole, the tail
    /// records the failure its retries ended on and lands no part. This is
    /// what `pkdump data refresh` — which since pd-lunn IS the landing run —
    /// leaves behind on such a night: `finalize` is called with `None`, because
    /// the run was not cut short, only the tail was (see `pkdump_derive::land`).
    fn land_with_dead_tail(root: &std::path::Path, date: &str, started: &str) -> String {
        let sink = RawLanding::new(Box::new(DirStore::new(root)), date, started);
        for (source, dataset, url) in [
            (Source::Tcgcsv, Dataset::Groups, "https://up/3/groups"),
            (Source::Tcgcsv, Dataset::Products, "https://up/3/1/products"),
            (Source::Tcgcsv, Dataset::Prices, "https://up/3/1/prices"),
        ] {
            sink.land(source, dataset, url, 200, PartFormat::Json, b"{}")
                .unwrap();
        }
        sink.record_failure(
            Source::PokemonTcgIo,
            Dataset::Sets,
            "https://up/sets",
            Some(502),
            "http 502 after 4 attempts",
        )
        .unwrap();
        sink.finalize(None).unwrap();
        sink.run_id().to_string()
    }

    fn zone(root: &std::path::Path) -> RawZone {
        RawZone::new(Box::new(DirStore::new(root)))
    }

    #[test]
    fn a_complete_date_chooses_every_required_dataset_and_no_more() {
        let tmp = tempfile::tempdir().unwrap();
        let run = land(tmp.path(), DATE, STARTED, false);
        let plan = choose(&zone(tmp.path()), DATE).unwrap();
        assert!(plan.partial.is_empty(), "a whole night is not partial");
        let chosen = plan.chosen;

        assert_eq!(chosen.len(), 4, "the four datasets every refresh fetches");
        assert!(chosen.iter().all(|c| c.run.run_id == run));
        // Optional and unlanded: absent from the plan rather than an error.
        assert!(!chosen.iter().any(|c| c.dataset == Dataset::Bulk));
        assert_eq!(clock_of(&chosen, DATE).unwrap().observed_date(), DATE);
    }

    /// The refusal the whole "two units" design turns on. A date nobody landed
    /// must not derive from whatever else is lying around.
    #[test]
    fn a_date_that_landed_nothing_refuses_rather_than_reaching_for_another() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), "2026-08-10", STARTED, false);
        let err = choose(&zone(tmp.path()), "2026-08-11")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no runs landed"), "{err}");
        assert!(err.contains("2026-08-11"), "{err}");
        assert!(err.contains("never falls back"), "{err}");
    }

    /// A run that died partway holds real bytes and an unknown fraction of the
    /// day. Building from it would produce a catalog that is quietly smaller.
    #[test]
    fn an_incomplete_run_refuses_and_names_what_it_found() {
        let tmp = tempfile::tempdir().unwrap();
        let run = land(tmp.path(), DATE, STARTED, true);
        let err = choose(&zone(tmp.path()), DATE).unwrap_err().to_string();
        assert!(err.contains("no complete run"), "{err}");
        assert!(err.contains(&run), "{err}");
    }

    /// …and a complete run followed by a failed retry still derives, from the
    /// complete one. Otherwise a retry that died would poison a good night.
    #[test]
    fn a_failed_retry_does_not_beat_the_complete_run_before_it() {
        let tmp = tempfile::tempdir().unwrap();
        let good = land(tmp.path(), DATE, STARTED, false);
        land(tmp.path(), DATE, "2026-08-11T05:10:00+00:00", true);
        let plan = choose(&zone(tmp.path()), DATE).unwrap();
        assert!(plan.chosen.iter().all(|c| c.run.run_id == good));
        assert!(plan.partial.is_empty(), "{:?}", plan.partial);
    }

    /// Two complete runs of one date, fetched at different instants. Every
    /// dataset resolves, and there is still no answer to "what time is it".
    #[test]
    fn runs_that_disagree_about_the_clock_refuse_naming_both() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), DATE, STARTED, false);
        let mut chosen = choose(&zone(tmp.path()), DATE).unwrap().chosen;
        // A second invocation landed `sets` an hour later — the shape a setup
        // and a refresh on one day leaves behind.
        chosen[3].run.manifest.started_at = "2026-08-11T05:51:02+00:00".to_string();

        let err = clock_of(&chosen, DATE).unwrap_err().to_string();
        assert!(err.contains("different instants"), "{err}");
        assert!(err.contains("04:51:02"), "{err}");
        assert!(err.contains("05:51:02"), "{err}");
        assert!(err.contains("pokemontcgio/sets"), "{err}");
    }

    /// A partition landed before the manifest recorded a clock cannot be
    /// derived from at all, and the refusal has to say which one and why.
    #[test]
    fn a_clockless_partition_refuses_by_name() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), DATE, "", false);
        let chosen = choose(&zone(tmp.path()), DATE).unwrap().chosen;
        let err = clock_of(&chosen, DATE).unwrap_err().to_string();
        assert!(err.contains("records no started_at"), "{err}");
        assert!(err.contains("tcgcsv/groups"), "{err}");
    }

    /// Provenance names the run, not the derive: two derives of one date
    /// produce the same rows apart from `derived_at`, which is what makes a
    /// rerun identifiable rather than invisible.
    #[test]
    fn provenance_names_the_runs_that_were_read() {
        let tmp = tempfile::tempdir().unwrap();
        let run = land(tmp.path(), DATE, STARTED, false);
        let chosen = choose(&zone(tmp.path()), DATE).unwrap().chosen;
        let clock = clock_of(&chosen, DATE).unwrap();
        let rows = provenance(&chosen, DATE, &clock);

        assert_eq!(rows.len(), 4);
        assert!(rows.iter().all(|r| r.run_id == run && r.complete));
        assert!(rows.iter().all(|r| r.parts == 1));
        assert_eq!(rows[0].source, "tcgcsv");
        assert_eq!(rows[0].dataset, "groups");
    }

    /// The UTC-midnight case, stated as an assertion rather than a comment: a
    /// run that started at 23:59 on the 10th and landed under the 11th's
    /// partition observed the 10th, and its rows must say so.
    #[test]
    fn the_observed_day_comes_from_the_fetch_not_the_partition() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), "2026-08-11", "2026-08-10T23:59:30+00:00", false);
        let chosen = choose(&zone(tmp.path()), "2026-08-11").unwrap().chosen;
        let clock = clock_of(&chosen, "2026-08-11").unwrap();

        assert_eq!(clock.observed_date(), "2026-08-10");
        let rows = provenance(&chosen, "2026-08-11", &clock);
        assert_eq!(rows[0].ingest_date, "2026-08-11");
        assert_eq!(rows[0].observed_at, "2026-08-10");
    }

    /// Optional means "may be absent", never "may be broken". A `bulk` run
    /// that died is evidence of a failed fetch, not of a night with nothing to
    /// fetch, and deriving past it would silently drop cards.
    ///
    /// `bulk` rather than `cards`, since pd-llbq: the two pokemontcg.io
    /// datasets ARE the tail, and a short prefix of one of those is a partial
    /// night rather than a refusal (see the two tests below). Every other
    /// optional dataset keeps this rule, which is why the test kept it too.
    #[test]
    fn an_optional_dataset_that_landed_badly_is_still_a_refusal() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), DATE, STARTED, false);
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), DATE, STARTED);
        sink.land(
            Source::PokemonTcgData,
            Dataset::Bulk,
            "https://up/bulk/cards.json",
            200,
            PartFormat::Json,
            b"{}",
        )
        .unwrap();
        sink.finalize(Some("http: 503 on the second file")).unwrap();

        let err = choose(&zone(tmp.path()), DATE).unwrap_err().to_string();
        assert!(err.contains("pokemon-tcg-data/bulk"), "{err}");
        assert!(err.contains("no complete run"), "{err}");
    }

    /// pd-llbq, the headline: the night `api.pokemontcg.io` is down.
    ///
    /// The tail fails every retry, so `pokemontcgio/sets` records the failure
    /// and reads incomplete while every TCGCSV prefix reads complete —
    /// `finalize` computes completeness per dataset, which is the honesty that
    /// makes this distinguishable at all. That date DERIVES, and says it is
    /// partial rather than refusing and paging.
    #[test]
    fn a_partition_short_only_in_the_tail_derives_and_says_it_is_partial() {
        let tmp = tempfile::tempdir().unwrap();
        let run = land_with_dead_tail(tmp.path(), DATE, STARTED);

        let plan = choose(&zone(tmp.path()), DATE).unwrap();
        assert_eq!(
            plan.chosen.len(),
            4,
            "every required dataset still resolved"
        );
        assert!(plan.chosen.iter().all(|c| c.run.run_id == run));
        assert!(plan.is_partial(), "a dead tail is a PARTIAL night");
        assert_eq!(plan.partial.len(), 1, "{:?}", plan.partial);
        assert!(
            plan.partial[0].contains("pokemontcgio/sets"),
            "{:?}",
            plan.partial
        );

        // And the provenance is honest about which half was short — the
        // column exists so a partial night is identifiable afterwards rather
        // than merely tolerated.
        let clock = clock_of(&plan.chosen, DATE).unwrap();
        let rows = provenance(&plan.chosen, DATE, &clock);
        let sets = rows.iter().find(|r| r.dataset == "sets").unwrap();
        assert!(!sets.complete, "the tail's own dataset is not complete");
        assert!(
            rows.iter()
                .filter(|r| r.source == "tcgcsv")
                .all(|r| r.complete),
            "the half a night cannot lose was whole"
        );
    }

    /// The tail dying BETWEEN two sets, which lands a complete `sets` and a
    /// short `cards`. Same night, same answer — `cards` is the tail too.
    #[test]
    fn a_short_cards_prefix_is_the_same_partial_night() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), DATE, STARTED, false);
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), DATE, STARTED);
        sink.land(
            Source::PokemonTcgIo,
            Dataset::Cards,
            "https://up/cards?q=set.id:fk1",
            200,
            PartFormat::Json,
            b"{}",
        )
        .unwrap();
        sink.record_failure(
            Source::PokemonTcgIo,
            Dataset::Cards,
            "https://up/cards?q=set.id:fk2",
            Some(502),
            "http 502",
        )
        .unwrap();
        sink.finalize(None).unwrap();

        let plan = choose(&zone(tmp.path()), DATE).unwrap();
        assert!(plan.is_partial(), "{plan:?}");
        assert_eq!(plan.partial.len(), 1, "{:?}", plan.partial);
        assert!(
            plan.partial[0].contains("pokemontcgio/cards"),
            "{:?}",
            plan.partial
        );
        assert_eq!(plan.chosen.len(), 5, "cards is in the plan, short and all");
    }

    /// The exemption is the TAIL's, not incompleteness's. A run that also lost
    /// TCGCSV is still refused — that shortfall is unknowable, and it is the
    /// half of a night that cannot be re-fetched tomorrow.
    #[test]
    fn a_dead_tail_does_not_excuse_a_short_tcgcsv_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        // finalize(Some(_)) is the shape of a run cut short: EVERY dataset is
        // marked incomplete, the tail's and TCGCSV's alike.
        land(tmp.path(), DATE, STARTED, true);
        let err = choose(&zone(tmp.path()), DATE).unwrap_err().to_string();
        assert!(err.contains("tcgcsv/groups"), "{err}");
        assert!(err.contains("no complete run"), "{err}");
    }

    /// Partial is about a run that landed SHORT. A date nothing landed at all
    /// is not a partial night, it is not a night — the refusal that the whole
    /// two-unit design turns on is unaffected by the exemption above.
    #[test]
    fn a_tail_that_landed_nothing_is_still_a_refusal_not_a_partial_night() {
        let tmp = tempfile::tempdir().unwrap();
        let sink = RawLanding::new(Box::new(DirStore::new(tmp.path())), DATE, STARTED);
        for (source, dataset, url) in [
            (Source::Tcgcsv, Dataset::Groups, "https://up/3/groups"),
            (Source::Tcgcsv, Dataset::Products, "https://up/3/1/products"),
            (Source::Tcgcsv, Dataset::Prices, "https://up/3/1/prices"),
        ] {
            sink.land(source, dataset, url, 200, PartFormat::Json, b"{}")
                .unwrap();
        }
        sink.finalize(None).unwrap();

        let err = choose(&zone(tmp.path()), DATE).unwrap_err().to_string();
        assert!(err.contains("pokemontcgio/sets"), "{err}");
        assert!(err.contains("no runs landed"), "{err}");
        assert!(err.contains("never falls back"), "{err}");
    }

    /// Every dataset the landing zone knows about is accounted for here. The
    /// exhaustive match in `requirement` is the compile-time half of this; the
    /// runtime half is that `choose` looks for all of them.
    #[test]
    fn every_dataset_is_looked_for() {
        let tmp = tempfile::tempdir().unwrap();
        land(tmp.path(), DATE, STARTED, false);
        let zone = zone(tmp.path());
        for &d in Dataset::ALL {
            let (source, _, _) = requirement(d);
            // Every one of them is a prefix the reader can be asked about —
            // the compile-time half of this is `requirement`'s exhaustive
            // match, which has no wildcard arm.
            zone.runs(source, d, DATE).unwrap();
        }
    }
}
