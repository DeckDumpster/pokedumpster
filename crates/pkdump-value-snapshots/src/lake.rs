//! The read side of the lake: the two price feeds at one pinned Nessie commit.
//!
//! The transliteration of `pkdump_lake.value_snapshots`'s `pin`, `market_prices`
//! and `sealed_market_prices`, over the kit's Iceberg client. Reads are batched
//! and reduced as they arrive for the reason the Python gives: the scan grows
//! with the lake's age, the result with the product count.

#![allow(clippy::needless_range_loop)] // rows index several parallel arrow columns

use std::collections::HashMap;

use arrow_array::{
    Array, Date32Array, Float64Array, Int64Array, LargeStringArray, RecordBatch, StringArray,
};
use dumpster_kit_iceberg::LakeCatalogConfig;
use futures::TryStreamExt;
use iceberg::expr::Reference;
use iceberg::spec::Datum;
use iceberg::{Catalog, ErrorKind, NamespaceIdent, TableIdent};

use crate::Refusal;

/// The one price type a collection is valued at.
pub const PRICE_TYPE: &str = "market";
/// …and the one a sealed lot falls back to, off the same single observation.
pub const SEALED_FALLBACK_PRICE_TYPE: &str = "mid";

pub type CardPrices = HashMap<(i64, String), f64>;
pub type SealedPrices = HashMap<i64, f64>;

/// Nessie's own v2 API base, derived from the Iceberg REST endpoint.
pub fn nessie_api_base(uri: &str) -> Result<String, Refusal> {
    let base = uri.trim_end_matches('/');
    match base.strip_suffix("/iceberg") {
        Some(root) => Ok(format!("{root}/api/v2")),
        None => Err(Refusal(format!(
            "expected an Iceberg REST endpoint ending in /iceberg, got {uri:?}"
        ))),
    }
}

/// The ref to read, resolved to a single commit **once** for the whole run, so
/// every tenant (and both feeds) are valued from the same catalog state.
pub async fn pin(config: &LakeCatalogConfig, explicit: Option<&str>) -> Result<String, Refusal> {
    let reference = explicit.unwrap_or(&config.reference);
    if reference.contains('@') {
        return Ok(reference.to_string());
    }
    let url = format!("{}/trees/{reference}", nessie_api_base(&config.uri)?);
    let body: serde_json::Value = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| Refusal(format!("could not resolve {reference} at {url}: {e}")))?
        .json()
        .await
        .map_err(|e| Refusal(format!("{url} did not answer JSON: {e}")))?;
    let hash = body["reference"]["hash"]
        .as_str()
        .ok_or_else(|| Refusal(format!("{url} carried no reference.hash")))?;
    Ok(format!("{reference}@{hash}"))
}

fn ident(name: &str) -> Result<TableIdent, Refusal> {
    let (ns, table) = name
        .split_once('.')
        .ok_or_else(|| Refusal(format!("{name:?} is not <namespace>.<table>")))?;
    Ok(TableIdent::new(
        NamespaceIdent::new(ns.to_string()),
        table.to_string(),
    ))
}

async fn scan(
    cat: &dyn Catalog,
    identifier: &str,
    missing: String,
    columns: &[&str],
    filter: iceberg::expr::Predicate,
) -> Result<(Vec<RecordBatch>, usize), Refusal> {
    let table = match cat.load_table(&ident(identifier)?).await {
        Ok(t) => t,
        Err(e)
            if matches!(
                e.kind(),
                ErrorKind::TableNotFound | ErrorKind::NamespaceNotFound
            ) =>
        {
            return Err(Refusal(missing));
        }
        Err(e) => return Err(Refusal(format!("loading {identifier}: {e}"))),
    };
    let plan = table
        .scan()
        .select(columns.iter().copied())
        .with_filter(filter)
        .build()
        .map_err(|e| Refusal(format!("planning the {identifier} scan: {e}")))?;
    let batches: Vec<RecordBatch> = plan
        .to_arrow()
        .await
        .map_err(|e| Refusal(format!("scanning {identifier}: {e}")))?
        .try_collect()
        .await
        .map_err(|e| Refusal(format!("reading {identifier}: {e}")))?;
    let n = batches.len();
    Ok((batches, n))
}

fn col<'a, T: 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a T, Refusal> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<T>())
        .ok_or_else(|| {
            Refusal(format!(
                "the lake's {name} column is not the type the lake wrote"
            ))
        })
}

/// A string column as owned `String`s, whichever arrow width iceberg handed back.
fn strings(batch: &RecordBatch, name: &str) -> Result<Vec<String>, Refusal> {
    let c = batch
        .column_by_name(name)
        .ok_or_else(|| Refusal(format!("the scan returned no {name} column")))?;
    if let Some(a) = c.as_any().downcast_ref::<StringArray>() {
        return Ok((0..a.len()).map(|i| a.value(i).to_string()).collect());
    }
    if let Some(a) = c.as_any().downcast_ref::<LargeStringArray>() {
        return Ok((0..a.len()).map(|i| a.value(i).to_string()).collect());
    }
    Err(Refusal(format!("the lake's {name} column is not a string")))
}

fn date_datum(date: &str) -> Result<Datum, Refusal> {
    Datum::date_from_str(date).map_err(|e| Refusal(format!("{date:?} is not YYYY-MM-DD: {e}")))
}

/// The newest market price per (product, sub_type) at or before `date`.
pub async fn market_prices(
    cat: &dyn Catalog,
    identifier: &str,
    date: &str,
) -> Result<CardPrices, Refusal> {
    let filter = Reference::new("price_type")
        .equal_to(Datum::string(PRICE_TYPE))
        .and(Reference::new("observed_date").less_than_or_equal_to(date_datum(date)?));
    let (batches, n) = scan(
        cat,
        identifier,
        format!(
            "no {identifier} at the pinned ref — build it first with \
             `pkdump-lake-build-prices --ingest-date <date>` (deploy/LAKE.md §6)"
        ),
        &[
            "tcgplayer_product_id",
            "sub_type_name",
            "price",
            "observed_date",
        ],
        filter,
    )
    .await?;

    let mut latest: HashMap<(i64, String), (i32, f64)> = HashMap::new();
    for batch in &batches {
        let ids: &Int64Array = col(batch, "tcgplayer_product_id")?;
        let subs = strings(batch, "sub_type_name")?;
        let prices: &Float64Array = col(batch, "price")?;
        let dates: &Date32Array = col(batch, "observed_date")?;
        for i in 0..batch.num_rows() {
            let key = (ids.value(i), subs[i].clone());
            let observed = dates.value(i);
            match latest.get(&key) {
                Some((seen, _)) if observed < *seen => {}
                _ => {
                    latest.insert(key, (observed, prices.value(i)));
                }
            }
        }
    }
    println!(
        "    {} product/sub-type price(s) from {n} batch(es)",
        latest.len()
    );
    Ok(latest.into_iter().map(|(k, (_, p))| (k, p)).collect())
}

/// The newest sealed price per product at or before `date`, resolved:
/// `COALESCE(market, mid)` between two quotes of ONE observation, never across
/// days. A product quoted neither on its newest day is absent, not zero.
pub async fn sealed_market_prices(
    cat: &dyn Catalog,
    identifier: &str,
    date: &str,
) -> Result<SealedPrices, Refusal> {
    let filter = Reference::new("observed_date").less_than_or_equal_to(date_datum(date)?);
    let (batches, n) = scan(
        cat,
        identifier,
        format!(
            "no {identifier} at the pinned ref — it is built by the same run that builds \
             catalog.prices, so a lake that has one and not the other was built by a \
             `pkdump-lake-build-prices` that predates pd-bbv7. Re-run it for this date."
        ),
        &[
            "tcgplayer_product_id",
            "price_type",
            "price",
            "observed_date",
        ],
        filter,
    )
    .await?;

    // Date is taken from EVERY row, including price types the rule does not
    // spend: a day that quoted only a low is still that product's newest day.
    let mut newest: HashMap<i64, (i32, HashMap<String, f64>)> = HashMap::new();
    for batch in &batches {
        let ids: &Int64Array = col(batch, "tcgplayer_product_id")?;
        let types = strings(batch, "price_type")?;
        let prices: &Float64Array = col(batch, "price")?;
        let dates: &Date32Array = col(batch, "observed_date")?;
        for i in 0..batch.num_rows() {
            let observed = dates.value(i);
            let entry = newest
                .entry(ids.value(i))
                .or_insert_with(|| (observed, HashMap::new()));
            if observed > entry.0 {
                *entry = (observed, HashMap::new());
            }
            if observed == entry.0
                && (types[i] == PRICE_TYPE || types[i] == SEALED_FALLBACK_PRICE_TYPE)
            {
                entry.1.insert(types[i].clone(), prices.value(i));
            }
        }
    }
    let resolved: SealedPrices = newest
        .into_iter()
        .filter_map(|(id, (_, quotes))| {
            quotes
                .get(PRICE_TYPE)
                .or_else(|| quotes.get(SEALED_FALLBACK_PRICE_TYPE))
                .map(|p| (id, *p))
        })
        .collect();
    println!(
        "    {} sealed product price(s) from {n} batch(es)",
        resolved.len()
    );
    Ok(resolved)
}
