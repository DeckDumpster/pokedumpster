//! Is `catalog.prices` still moving? (`pkdump_lake.freshness`, ported.)
//!
//! The alarm is the **age of the newest partition** and nothing else: the build
//! tolerates an incomplete night, so a per-day completeness alarm would page most
//! nights. One table is enough for two, because one build writes both price
//! tables or neither. See the Python module for the long form of both arguments.
//!
//! Reads table *metadata*, never data files: the live data files' partition
//! values and row counts come from the current snapshot's manifests, which is
//! what PyIceberg's `inspect.partitions()` aggregates.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use dumpster_kit_iceberg::{Error as LakeError, LakeCatalogConfig};
use iceberg::spec::{DataContentType, Literal, PrimitiveLiteral};
use iceberg::table::Table;
use iceberg::{Catalog, ErrorKind, NamespaceIdent, TableIdent};

pub const DEFAULT_TABLE: &str = "catalog.prices";
pub const PARTITION_FIELD: &str = "observed_date";
pub const DEFAULT_MAX_AGE_DAYS: i64 = 2;

pub const EXIT_FRESH: i32 = 0;
pub const EXIT_ERROR: i32 = 1;
pub const EXIT_STALE: i32 = 3;

#[derive(Debug, thiserror::Error)]
pub enum FreshnessError {
    #[error("{0}")]
    Shape(String),
    #[error(transparent)]
    Lake(#[from] LakeError),
    #[error("iceberg: {0}")]
    Iceberg(#[from] iceberg::Error),
}

/// What the table said, before it is judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Newest {
    /// No such table (or namespace): nothing has ever built it.
    NoTable,
    /// The table exists and holds no live partition.
    NoPartition,
    Day(NaiveDate),
}

/// One live partition: its day and the rows its live data files hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub day: i32,
    pub record_count: u64,
}

fn epoch() -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch")
}

fn day_to_date(days: i32) -> NaiveDate {
    epoch() + chrono::Duration::days(days as i64)
}

/// The newest day among partitions holding rows. An emptied partition is not a
/// day of prices.
pub fn newest_of(partitions: &[Partition]) -> Option<NaiveDate> {
    partitions
        .iter()
        .filter(|p| p.record_count > 0)
        .map(|p| p.day)
        .max()
        .map(day_to_date)
}

pub fn parse_ident(identifier: &str) -> TableIdent {
    let mut parts: Vec<String> = identifier.split('.').map(str::to_string).collect();
    let name = parts.pop().unwrap_or_default();
    TableIdent::new(
        NamespaceIdent::from_vec(parts).expect("non-empty namespace"),
        name,
    )
}

/// The live partitions of `table`, aggregated from the current snapshot's data
/// manifests.
pub async fn partitions(table: &Table, identifier: &str) -> Result<Vec<Partition>, FreshnessError> {
    let Some(snapshot) = table.metadata().current_snapshot() else {
        return Ok(vec![]);
    };
    let list = table.manifest_list_reader(snapshot).load().await?;
    let mut by_day: BTreeMap<i32, u64> = BTreeMap::new();
    for mf in list.entries() {
        if mf.content != iceberg::spec::ManifestContentType::Data {
            continue;
        }
        let manifest = mf.load_manifest(table.file_io()).await?;
        let spec = manifest.metadata().partition_spec();
        let Some(pos) = spec.fields().iter().position(|f| f.name == PARTITION_FIELD) else {
            let names: Vec<_> = spec.fields().iter().map(|f| f.name.clone()).collect();
            return Err(FreshnessError::Shape(format!(
                "{identifier} is not partitioned by {PARTITION_FIELD} — the manifest spec gave \
                 {names:?}. This job judges the age of a day, so a table partitioned some other \
                 way cannot be judged here."
            )));
        };
        for entry in manifest.entries() {
            let file = entry.data_file();
            if !entry.is_alive() || file.content_type() != DataContentType::Data {
                continue;
            }
            let day = match file.partition().fields().get(pos) {
                Some(Some(Literal::Primitive(PrimitiveLiteral::Int(d)))) => *d,
                other => {
                    return Err(FreshnessError::Shape(format!(
                        "{identifier}: partition value for {PARTITION_FIELD} was {other:?}, not a date"
                    )));
                }
            };
            *by_day.entry(day).or_default() += file.record_count();
        }
    }
    Ok(by_day
        .into_iter()
        .map(|(day, record_count)| Partition { day, record_count })
        .collect())
}

pub async fn newest_partition(
    cat: &dyn Catalog,
    identifier: &str,
) -> Result<Newest, FreshnessError> {
    let ident = parse_ident(identifier);
    let table = match cat.load_table(&ident).await {
        Ok(t) => t,
        Err(e)
            if matches!(
                e.kind(),
                ErrorKind::TableNotFound | ErrorKind::NamespaceNotFound
            ) =>
        {
            return Ok(Newest::NoTable);
        }
        Err(e) => return Err(e.into()),
    };
    let parts = partitions(&table, identifier).await?;
    Ok(match newest_of(&parts) {
        Some(d) => Newest::Day(d),
        None => Newest::NoPartition,
    })
}

/// The verdict as the exact lines the Python prints, and the exit status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
    pub code: i32,
}

/// `judge`, with the question already answered. Pure: the text is the contract.
pub fn judge(
    identifier: &str,
    resolved_ref: &str,
    as_of: NaiveDate,
    max_age_days: i64,
    newest: &Newest,
) -> Verdict {
    let mut v = Verdict {
        stdout: vec![format!(
            "==> {identifier} at {resolved_ref}: newest partition vs {as_of}"
        )],
        stderr: vec![],
        code: EXIT_FRESH,
    };
    let newest = match newest {
        Newest::NoTable => {
            v.stderr.push(format!(
                "prices-age: STALE — there is no {identifier} at {resolved_ref}. Nothing has ever \
                 built it: bash deploy/prices.sh <instance> (deploy/LAKE.md §6)"
            ));
            v.code = EXIT_STALE;
            return v;
        }
        Newest::NoPartition => {
            v.stderr.push(format!(
                "prices-age: STALE — {identifier} at {resolved_ref} holds no price partition at all"
            ));
            v.code = EXIT_STALE;
            return v;
        }
        Newest::Day(d) => *d,
    };
    let age = (as_of - newest).num_days();
    v.stdout.push(format!(
        "    newest observed_date={newest} ({age} day(s) behind {as_of})"
    ));
    if age > max_age_days {
        v.stderr.push(format!(
            "prices-age: STALE — the newest {identifier} partition is {newest}, {age} days behind \
             {as_of} (limit {max_age_days}). Collections are still being valued nightly, from \
             prices that stopped arriving."
        ));
        v.code = EXIT_STALE;
        return v;
    }
    if age < 0 {
        v.stdout.push(format!(
            "prices-age: FRESH — {identifier} holds {newest}, which is AFTER {as_of}. Nothing is \
             stale; something dated a partition ahead of the caller."
        ));
        return v;
    }
    v.stdout.push(format!(
        "prices-age: FRESH — {identifier} is {age} day(s) behind (limit {max_age_days})"
    ));
    v
}

/// Open the lake and judge `identifier` against `as_of`.
pub async fn run(
    identifier: &str,
    as_of: NaiveDate,
    max_age_days: i64,
    reference: Option<&str>,
) -> Result<Verdict, FreshnessError> {
    let mut config = LakeCatalogConfig::from_env()?;
    if let Some(r) = reference {
        config.reference = r.to_string();
    }
    let cat = dumpster_kit_iceberg::open(&config).await?;
    let newest = newest_partition(&cat, identifier).await?;
    Ok(judge(
        identifier,
        &config.reference,
        as_of,
        max_age_days,
        &newest,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }

    #[test]
    fn an_emptied_partition_is_not_a_day_of_prices() {
        let p = [
            Partition {
                day: 20000,
                record_count: 5,
            },
            Partition {
                day: 20005,
                record_count: 0,
            },
        ];
        assert_eq!(newest_of(&p), Some(day_to_date(20000)));
        assert_eq!(newest_of(&[]), None);
    }

    #[test]
    fn epoch_days_round_trip_to_the_date() {
        assert_eq!(day_to_date(20742), d("2026-10-16"));
    }

    #[test]
    fn the_age_boundary_is_inclusive() {
        let at = |n| {
            judge(
                "catalog.prices",
                "main",
                d("2026-08-13"),
                2,
                &Newest::Day(d("2026-08-13") - chrono::Duration::days(n)),
            )
        };
        assert_eq!(at(2).code, EXIT_FRESH);
        assert_eq!(at(3).code, EXIT_STALE);
    }

    #[test]
    fn identifiers_split_on_the_last_dot() {
        let i = parse_ident("catalog.prices");
        assert_eq!(i.name(), "prices");
        assert_eq!(i.namespace().to_url_string(), "catalog");
    }
}
