//! `pkdump-lake-verify-prices` — `catalog.prices` and `catalog.sealed_prices` against the
//! `shared.sqlite` built from the same bytes, for one `observed_date`.
//!
//! The Rust replacement for `lake/src/pkdump_lake/verify.py` (sp-4k7mxi.4): same three halves,
//! same output, same exit status (0 agree, 1 mismatch). Single cards must agree exactly; every
//! sealed price SQLite kept must be in `catalog.prices` (the lake may hold more); and
//! `catalog.sealed_prices` must agree with `shared.sealed_prices` exactly, in BOTH directions.
//!
//! Run it against a COPY of a real catalog. The connection is `query_only` and issues nothing
//! but `SELECT`, but SQLite still opens a WAL database at file level.

#[allow(dead_code)]
#[path = "prices_core.rs"]
mod prices_core;

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::{Context, Result};
use arrow_array::cast::AsArray;
use arrow_array::types::{Float64Type, Int64Type};
use clap::Parser;
use dumpster_kit_iceberg::{LakeCatalogConfig, prices};
use futures::TryStreamExt;
use iceberg::expr::Reference;
use iceberg::spec::Datum;
use iceberg::{Catalog, TableIdent};

/// `(price_type, shared.sealed_prices column)`, in the order the build writes them.
const SEALED_COLUMNS: [(&str, &str); 5] = [
    ("low", "low_price"),
    ("mid", "mid_price"),
    ("high", "high_price"),
    ("market", "market_price"),
    ("directLow", "direct_low_price"),
];

type Single = (i64, String, String);
type Sealed = (i64, String);
type SqliteSides = (BTreeMap<Single, f64>, BTreeMap<Sealed, f64>, BTreeSet<i64>);

#[derive(Parser)]
#[command(name = "pkdump-lake-verify-prices", version, about)]
struct Cli {
    /// Path to a shared.sqlite (opened query_only).
    #[arg(long)]
    sqlite: String,
    /// YYYY-MM-DD.
    #[arg(long)]
    observed_date: String,
    /// The raw/ partition whose products listing classifies sealed product
    /// (default: --observed-date).
    #[arg(long, value_name = "YYYY-MM-DD")]
    ingest_date: Option<String>,
    /// How many matched rows to print as evidence.
    #[arg(long, default_value_t = 10)]
    sample: usize,
}

/// The sealed product ids the night's own raw products listing classifies (sp-gc251u).
/// Incomplete runs are read, as the Python did: this only decides which rows are history.
fn night_sealed_ids(ingest: &str) -> Result<BTreeSet<i64>> {
    use pkdump_site::{PRODUCTS, TCGCSV};
    let zone = dumpster_kit_lake::open_reader(&pkdump_site::lake_env())?;
    let runs = prices_core::select_runs(&zone.runs(TCGCSV, PRODUCTS, ingest)?, true)?;
    let mut fetch = |p: &dumpster_kit_lake::PartRecord| -> Result<Vec<u8>> { Ok(zone.payload(p)?) };
    let (ids, _runs) = prices_core::sealed_product_ids(&runs, ingest, &mut fetch)?;
    Ok(ids.into_iter().collect())
}

/// Sealed rows only SQLite holds, split into `(defects, history_only)`.
///
/// `shared.sealed_products` accumulates across nights (upsert, never delete) while
/// `catalog.sealed_prices` is classified from one night's listing, so a product sealed on an
/// earlier night and absent from tonight's listing is in SQLite and cannot be in the lake. Those
/// are history, reported not failed. A row for a product the night DOES list stays a defect.
/// `None` means no classification: everything is a defect.
fn split_unclassifiable(
    missing: BTreeSet<Sealed>,
    night: Option<&BTreeSet<i64>>,
) -> (BTreeSet<Sealed>, BTreeSet<Sealed>) {
    let Some(night) = night else {
        return (missing, BTreeSet::new());
    };
    missing.into_iter().partition(|k| night.contains(&k.0))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let day = chrono::NaiveDate::parse_from_str(&cli.observed_date, "%Y-%m-%d")
        .context("--observed-date")?
        .to_string();
    let rt = tokio::runtime::Runtime::new()?;
    let (lake, lake_sealed_table) = rt.block_on(lake_sides(&day))?;
    let (singles, sealed, sealed_ids) = sqlite_sides(&cli.sqlite, &day)?;
    let night = night_sealed_ids(cli.ingest_date.as_deref().unwrap_or(&day))?;
    let code = compare(
        &day,
        &lake,
        &lake_sealed_table,
        &singles,
        &sealed,
        &sealed_ids,
        Some(&night),
        cli.sample,
    );
    std::process::exit(code);
}

async fn lake_sides(day: &str) -> Result<(BTreeMap<Single, f64>, BTreeMap<Sealed, f64>)> {
    let cat = dumpster_kit_iceberg::open(&LakeCatalogConfig::from_env()?).await?;
    let mut singles = BTreeMap::new();
    for batch in scan_day(&cat, &prices::prices_ident(), day).await? {
        let ids = batch
            .column_by_name("tcgplayer_product_id")
            .context("no tcgplayer_product_id")?
            .as_primitive::<Int64Type>();
        let subs = batch
            .column_by_name("sub_type_name")
            .context("no sub_type_name")?
            .as_string::<i32>();
        let types = batch
            .column_by_name("price_type")
            .context("no price_type")?
            .as_string::<i32>();
        let vals = batch
            .column_by_name("price")
            .context("no price")?
            .as_primitive::<Float64Type>();
        for i in 0..batch.num_rows() {
            singles.insert(
                (
                    ids.value(i),
                    subs.value(i).to_string(),
                    types.value(i).to_string(),
                ),
                vals.value(i),
            );
        }
    }
    let mut sealed = BTreeMap::new();
    for batch in scan_day(&cat, &prices::sealed_ident(), day).await? {
        let ids = batch
            .column_by_name("tcgplayer_product_id")
            .context("no tcgplayer_product_id")?
            .as_primitive::<Int64Type>();
        let types = batch
            .column_by_name("price_type")
            .context("no price_type")?
            .as_string::<i32>();
        let vals = batch
            .column_by_name("price")
            .context("no price")?
            .as_primitive::<Float64Type>();
        for i in 0..batch.num_rows() {
            sealed.insert((ids.value(i), types.value(i).to_string()), vals.value(i));
        }
    }
    Ok((singles, sealed))
}

async fn scan_day(
    cat: &dyn Catalog,
    ident: &TableIdent,
    day: &str,
) -> Result<Vec<arrow_array::RecordBatch>> {
    let table = cat.load_table(ident).await?;
    let scan = table
        .scan()
        .with_filter(Reference::new("observed_date").equal_to(Datum::date_from_str(day)?))
        .build()?;
    Ok(scan.to_arrow().await?.try_collect().await?)
}

/// `prices`, `sealed_prices` unpivoted, and the sealed product ids.
fn sqlite_sides(path: &str, day: &str) -> Result<SqliteSides> {
    // Not read-only: these are WAL databases and SQLite cannot read one through a read-only
    // connection at all. `query_only` refuses to change anything while SQLite keeps its -shm.
    let conn = rusqlite::Connection::open(path)?;
    conn.pragma_update(None, "query_only", true)?;
    let mut singles = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT tcgplayer_product_id, sub_type_name, price_type, price FROM prices WHERE observed_at = ?",
    )?;
    for r in stmt.query_map([day], |r| {
        Ok((
            (
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ),
            r.get::<_, f64>(3)?,
        ))
    })? {
        let (k, v) = r?;
        singles.insert(k, v);
    }
    let cols = SEALED_COLUMNS.map(|(_, c)| c).join(", ");
    let mut sealed = BTreeMap::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT tcgplayer_product_id, {cols} FROM sealed_prices WHERE observed_at = ?"
    ))?;
    for r in stmt.query_map([day], |r| {
        let id: i64 = r.get(0)?;
        let mut vals = Vec::new();
        for i in 0..SEALED_COLUMNS.len() {
            vals.push(r.get::<_, Option<f64>>(i + 1)?);
        }
        Ok((id, vals))
    })? {
        let (id, vals) = r?;
        for ((price_type, _), v) in SEALED_COLUMNS.iter().zip(vals) {
            if let Some(v) = v {
                sealed.insert((id, price_type.to_string()), v);
            }
        }
    }
    let mut ids = BTreeSet::new();
    let mut stmt = conn.prepare("SELECT product_id FROM sealed_products")?;
    for r in stmt.query_map([], |r| r.get::<_, i64>(0))? {
        ids.insert(r?);
    }
    Ok((singles, sealed, ids))
}

fn report(differences: &[String], label: &str) {
    println!("    {} {label}", differences.len());
    for line in differences.iter().take(10) {
        println!("      {line}");
    }
    if differences.len() > 10 {
        println!("      … and {} more", differences.len() - 10);
    }
}

#[allow(clippy::too_many_arguments)]
/// The comparison. Pure over the four maps, so the tests drive it without a lake. Returns the
/// exit status.
fn compare(
    day: &str,
    lake: &BTreeMap<Single, f64>,
    lake_sealed_table: &BTreeMap<Sealed, f64>,
    singles: &BTreeMap<Single, f64>,
    sealed: &BTreeMap<Sealed, f64>,
    sealed_ids: &BTreeSet<i64>,
    night_sealed: Option<&BTreeSet<i64>>,
    sample: usize,
) -> i32 {
    println!("==> observed_date={day}");
    println!("    lake  {:<24} {:>9} rows", "catalog.prices", lake.len());
    println!(
        "    sqlite prices                     {:>9} rows",
        singles.len()
    );
    println!(
        "    sqlite sealed_prices (unpivoted)  {:>9} rows",
        sealed.len()
    );

    let (lake_sealed, lake_singles): (BTreeMap<_, _>, BTreeMap<_, _>) = lake
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .partition(|(k, _)| sealed_ids.contains(&k.0));
    println!(
        "    of which single-card              {:>9} rows",
        lake_singles.len()
    );
    println!(
        "    of which sealed                   {:>9} rows",
        lake_sealed.len()
    );

    let mut failures = 0;

    println!("==> single cards: the two must agree exactly");
    if !exact(singles, &lake_singles, "sqlite", "lake") {
        failures += 1;
    } else {
        println!("    ok   {} rows identical, value for value", singles.len());
    }

    println!("==> sealed products: every price sqlite kept must be in the lake");
    // SQLite's sealed row has no sub_type, so a product quoted under two sub-types collapses
    // to one row there and stays two here. The lake holding more is expected; less is not.
    let mut by_product_type: HashMap<Sealed, Vec<f64>> = HashMap::new();
    for ((pid, _sub, pt), v) in &lake_sealed {
        by_product_type
            .entry((*pid, pt.clone()))
            .or_default()
            .push(*v);
    }
    let lost: Vec<String> = sealed
        .iter()
        .filter(|(k, v)| !by_product_type.get(*k).is_some_and(|l| l.contains(v)))
        .map(|(k, v)| {
            let mut have = by_product_type.get(k).cloned().unwrap_or_default();
            have.sort_by(f64::total_cmp);
            format!("{k:?}: sqlite {v:?}, lake has {have:?}")
        })
        .collect();
    if lost.is_empty() {
        println!(
            "    ok   all {} sealed prices present in the lake",
            sealed.len()
        );
    } else {
        failures += 1;
        report(&lost, "sealed price(s) sqlite holds and the lake does not");
    }
    if lake_sealed.len() > sealed.len() {
        println!(
            "    note {} sealed price row(s) exist only in the lake — sealed_prices is \
             UNIQUE(product, observed_at) and has no sub_type_name, so a sealed product quoted \
             under more than one sub-type loses all but the first in shared.sqlite. The lake \
             keeps them.",
            lake_sealed.len() - sealed.len()
        );
    }

    println!("==> catalog.sealed_prices: the two must agree exactly, both ways");
    println!(
        "    lake  {:<24} {:>9} rows",
        "catalog.sealed_prices",
        lake_sealed_table.len()
    );
    let only_sqlite: BTreeSet<Sealed> = sealed
        .keys()
        .filter(|k| !lake_sealed_table.contains_key(*k))
        .cloned()
        .collect();
    let (_defects, history) = split_unclassifiable(only_sqlite, night_sealed);
    if !history.is_empty() {
        let products: BTreeSet<i64> = history.iter().map(|k| k.0).collect();
        let shown: Vec<_> = products.iter().take(20).collect();
        println!(
            "    note {} sealed row(s) over {} product(s) exist only in sqlite: \
             shared.sealed_products is accumulated across nights and these products are not \
             sealed in tonight's raw products listing, so the lake (built from that listing \
             alone) does not classify them. Products: {shown:?}{}",
            history.len(),
            products.len(),
            if products.len() > 20 { " …" } else { "" }
        );
    }
    let comparable: BTreeMap<Sealed, f64> = sealed
        .iter()
        .filter(|(k, _)| !history.contains(*k))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    if !exact(&comparable, lake_sealed_table, "sqlite", "lake") {
        failures += 1;
    } else {
        println!(
            "    ok   {} sealed rows identical, value for value",
            comparable.len()
        );
    }

    println!("==> sample of {sample} matched rows");
    for (k, v) in singles.iter().take(sample) {
        println!("    {k:?} price {v:?} (both sides)");
    }

    if failures > 0 {
        println!("==> MISMATCH");
        return 1;
    }
    println!("==> catalog.prices, catalog.sealed_prices and shared.sqlite agree");
    0
}

/// Equal in both directions and in value; prints what differs. True when identical.
fn exact<K: Ord + std::fmt::Debug>(
    sqlite: &BTreeMap<K, f64>,
    lake: &BTreeMap<K, f64>,
    left: &str,
    right: &str,
) -> bool {
    let missing: Vec<String> = sqlite
        .keys()
        .filter(|k| !lake.contains_key(*k))
        .map(|k| format!("{k:?} in {left}, not in the {right}"))
        .collect();
    let extra: Vec<String> = lake
        .keys()
        .filter(|k| !sqlite.contains_key(*k))
        .map(|k| format!("{k:?} in the {right}, not in {left}"))
        .collect();
    let differing: Vec<String> = sqlite
        .iter()
        .filter(|(k, v)| lake.get(*k).is_some_and(|l| l != *v))
        .map(|(k, v)| format!("{k:?}: {left} {v:?} vs {right} {:?}", lake[k]))
        .collect();
    if missing.is_empty() && extra.is_empty() && differing.is_empty() {
        return true;
    }
    report(&missing, &format!("row(s) only in {left}"));
    report(&extra, &format!("row(s) only in the {right}"));
    report(&differing, "row(s) whose price differs");
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: i64, sub: &str, pt: &str) -> Single {
        (id, sub.into(), pt.into())
    }

    #[test]
    fn sealed_columns_follow_the_builds_price_type_order() {
        let build: Vec<&str> = prices_core::PRICE_TYPES.iter().map(|(t, _)| *t).collect();
        let verify: Vec<&str> = SEALED_COLUMNS.iter().map(|(t, _)| *t).collect();
        assert_eq!(build, verify);
    }

    #[test]
    fn agreement_is_zero_and_every_direction_of_disagreement_is_one() {
        let ids = BTreeSet::from([9]);
        let singles = BTreeMap::from([(s(1, "Normal", "market"), 1.5)]);
        let sealed = BTreeMap::from([((9, "market".to_string()), 20.0)]);
        let lake = BTreeMap::from([
            (s(1, "Normal", "market"), 1.5),
            (s(9, "Normal", "market"), 20.0),
            (s(9, "Foil", "market"), 22.0),
        ]);
        let run = |lake: &BTreeMap<Single, f64>,
                   ls: &BTreeMap<Sealed, f64>,
                   sealed: &BTreeMap<Sealed, f64>| {
            compare("2026-08-11", lake, ls, &singles, sealed, &ids, None, 1)
        };
        // The lake keeping the Foil sub-type row beside the one SQLite kept is expected.
        assert_eq!(run(&lake, &sealed, &sealed), 0);
        // sealed table holding an extra row, or a different value, or missing one: all red.
        let mut extra = sealed.clone();
        extra.insert((9, "low".into()), 1.0);
        assert_eq!(run(&lake, &extra, &sealed), 1);
        let mut diff = sealed.clone();
        diff.insert((9, "market".into()), 21.0);
        assert_eq!(run(&lake, &diff, &sealed), 1);
        assert_eq!(run(&lake, &BTreeMap::new(), &sealed), 1);
        // A single the lake lost.
        assert_eq!(run(&BTreeMap::new(), &sealed, &sealed), 1);
        // A sealed price SQLite kept that the lake's prices table does not hold.
        let mut gone = lake.clone();
        gone.remove(&s(9, "Normal", "market"));
        gone.remove(&s(9, "Foil", "market"));
        assert_eq!(run(&gone, &sealed, &sealed), 1);
    }

    #[test]
    fn history_only_sealed_rows_are_reported_not_failed() {
        let ids = BTreeSet::from([1, 900]);
        let singles = BTreeMap::new();
        let sealed = BTreeMap::from([
            ((1, "market".to_string()), 1.0),
            ((1, "low".to_string()), 1.0),
            ((900, "low".to_string()), 2.0),
        ]);
        let lake_sealed = BTreeMap::from([
            ((1, "market".to_string()), 1.0),
            ((1, "low".to_string()), 1.0),
        ]);
        let lake: BTreeMap<Single, f64> = sealed
            .iter()
            .map(|((p, t), v)| (s(*p, "Normal", t), *v))
            .collect();
        let run = |ls: &BTreeMap<Sealed, f64>, night: Option<&BTreeSet<i64>>| {
            compare("2026-10-16", &lake, ls, &singles, &sealed, &ids, night, 1)
        };
        // 900 is not sealed tonight: history, passes.
        assert_eq!(run(&lake_sealed, Some(&BTreeSet::from([1]))), 0);
        // Without a classification the same gap is a defect.
        assert_eq!(run(&lake_sealed, None), 1);
        // A listed product missing from the lake still fails.
        let mut short = lake_sealed.clone();
        short.remove(&(1, "low".to_string()));
        assert_eq!(run(&short, Some(&BTreeSet::from([1]))), 1);
    }
}
