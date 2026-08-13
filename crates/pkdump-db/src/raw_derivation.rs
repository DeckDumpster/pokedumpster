//! Which raw runs produced this catalog (pd-1uem).
//!
//! Landing and deriving are separate scheduled units, which is a feature —
//! a derive can run against yesterday's raw when tonight's landing failed —
//! and the trap beside it is that **yesterday's raw quietly derives today's
//! catalog and looks current**. Three things keep the feature and drop the
//! trap. Two live in the offline job: it takes an explicit `--ingest-date`
//! and never defaults one from the clock, and it refuses a partition that
//! is absent or incomplete rather than falling back to the newest available.
//!
//! This is the third. After a derive succeeds, the catalog carries a row per
//! `(ingest_date, source, dataset)` naming the run ULID that was replayed,
//! how many parts it held, and when the derive ran. So "which bytes is this
//! catalog made of" is a query rather than a reconstruction from timer logs,
//! and a rerun is *identifiable* rather than merely tolerated.
//!
//! Nothing on the serving path reads this table, deliberately. It is a record
//! for an operator, not an input to a decision — a table the app depended on
//! would be one more thing a restore has to get right.

use rusqlite::{Connection, params};

use crate::error::Result;

/// One raw partition a derive consumed. Mirrors the table 1:1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDerivation {
    /// The `ingest_date=` partition the bytes were read from.
    pub ingest_date: String,
    /// The `source=` partition value.
    pub source: String,
    /// The `dataset=` partition value.
    pub dataset: String,
    /// The `run=` ULID that was replayed.
    pub run_id: String,
    /// How many payloads that run landed under this prefix.
    pub parts: i64,
    /// The manifest's `complete` flag. Always true today — the job refuses an
    /// incomplete run — and recorded anyway so the row states the fact rather
    /// than implying it from the job's current policy.
    pub complete: bool,
    /// The run's clock day, `YYYY-MM-DD`. Distinct from `ingest_date`: they
    /// differ for exactly the run that crossed UTC midnight.
    pub observed_at: String,
}

/// Replace the provenance for one `ingest_date` with `rows`.
///
/// Delete-then-insert, in one transaction, for the same reason the tenant
/// transform works that way: re-deriving a date has to mean *replacing* that
/// date, so twice equals once. Accumulating a row per attempt would make the
/// table grow with every rerun and leave "which run is this catalog actually
/// made of" ambiguous — which is the one question it exists to answer.
///
/// `derived_at` is passed in rather than read here: this module writes what
/// it is told, and the job that ran is the thing that knows when it ran.
pub fn record(
    conn: &mut Connection,
    ingest_date: &str,
    derived_at: &str,
    rows: &[RawDerivation],
) -> Result<usize> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM raw_derivation WHERE ingest_date = ?1",
        params![ingest_date],
    )?;
    for r in rows {
        tx.execute(
            "INSERT INTO raw_derivation \
               (ingest_date, source, dataset, run_id, parts, complete, observed_at, derived_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                r.ingest_date,
                r.source,
                r.dataset,
                r.run_id,
                r.parts,
                r.complete as i64,
                r.observed_at,
                derived_at,
            ],
        )?;
    }
    tx.commit()?;
    Ok(rows.len())
}

/// Every partition recorded for one `ingest_date`, in a deterministic order.
pub fn for_date(conn: &Connection, ingest_date: &str) -> Result<Vec<RawDerivation>> {
    let mut stmt = conn.prepare(
        "SELECT ingest_date, source, dataset, run_id, parts, complete, observed_at \
           FROM raw_derivation WHERE ingest_date = ?1 ORDER BY source, dataset",
    )?;
    let rows = stmt.query_map(params![ingest_date], |r| {
        Ok(RawDerivation {
            ingest_date: r.get(0)?,
            source: r.get(1)?,
            dataset: r.get(2)?,
            run_id: r.get(3)?,
            parts: r.get(4)?,
            complete: r.get::<_, i64>(5)? != 0,
            observed_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(dataset: &str, run: &str, parts: i64) -> RawDerivation {
        RawDerivation {
            ingest_date: "2026-08-11".to_string(),
            source: "tcgcsv".to_string(),
            dataset: dataset.to_string(),
            run_id: run.to_string(),
            parts,
            complete: true,
            observed_at: "2026-08-11".to_string(),
        }
    }

    fn catalog() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("schema_shared.sql"))
            .unwrap();
        conn.transaction().unwrap().commit().unwrap();
        conn
    }

    /// The property the whole table exists for: a rerun REPLACES the date's
    /// provenance. A second derive that appended would leave two run ids for
    /// one date and no way to tell which one the catalog is made of.
    #[test]
    fn re_deriving_a_date_replaces_its_provenance() {
        let mut conn = catalog();
        record(
            &mut conn,
            "2026-08-11",
            "2026-08-11T05:00:00Z",
            &[row("groups", "RUN_A", 1), row("prices", "RUN_A", 4)],
        )
        .unwrap();
        record(
            &mut conn,
            "2026-08-11",
            "2026-08-12T05:00:00Z",
            &[row("groups", "RUN_B", 1), row("prices", "RUN_B", 4)],
        )
        .unwrap();

        let back = for_date(&conn, "2026-08-11").unwrap();
        assert_eq!(back.len(), 2);
        assert!(back.iter().all(|r| r.run_id == "RUN_B"));
    }

    /// …and it replaces THAT date only. A derive of one date must not erase
    /// the record of another; the table is a history across dates and a
    /// replacement within one.
    #[test]
    fn another_date_is_untouched() {
        let mut conn = catalog();
        let mut old = row("groups", "RUN_OLD", 1);
        old.ingest_date = "2026-08-10".to_string();
        old.observed_at = "2026-08-10".to_string();
        record(&mut conn, "2026-08-10", "2026-08-10T05:00:00Z", &[old]).unwrap();
        record(
            &mut conn,
            "2026-08-11",
            "2026-08-11T05:00:00Z",
            &[row("groups", "RUN_NEW", 1)],
        )
        .unwrap();

        assert_eq!(for_date(&conn, "2026-08-10").unwrap()[0].run_id, "RUN_OLD");
        assert_eq!(for_date(&conn, "2026-08-11").unwrap()[0].run_id, "RUN_NEW");
    }

    /// `observed_at` is the run's clock day and `ingest_date` is the
    /// partition. The UTC-midnight run is the one where they differ, and the
    /// row has to be able to say so.
    #[test]
    fn the_observation_day_can_differ_from_the_partition() {
        let mut conn = catalog();
        let mut crossed = row("prices", "RUN_MIDNIGHT", 2);
        crossed.observed_at = "2026-08-10".to_string();
        record(&mut conn, "2026-08-11", "2026-08-11T00:00:30Z", &[crossed]).unwrap();

        let back = &for_date(&conn, "2026-08-11").unwrap()[0];
        assert_eq!(back.ingest_date, "2026-08-11");
        assert_eq!(back.observed_at, "2026-08-10");
    }
}
