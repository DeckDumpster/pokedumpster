//! Raw TCGCSV bytes into `catalog.prices` / `catalog.sealed_prices` rows.
//!
//! A transliteration of `lake/src/pkdump_lake/prices.py` and the `select_runs` of `raw.py`,
//! function for function, with no IO of its own: payloads arrive through a closure, so the
//! parity tests in `tests/prices_parity.rs` feed it the same bytes the Python was recorded
//! on and compare rows. Every rule below is a decision the Python states; the doc comments
//! here carry the ones that are easy to get wrong in a port.

use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Context, Result, bail};
use dumpster_kit_iceberg::prices::{PriceRow, SealedPriceRow};
use dumpster_kit_lake::{PartRecord, Run};
use serde_json::Value;

/// TCGplayer's category for Pokémon Japan. Its single cards are discriminated by `CardType`
/// rather than `Number` (~2.4k vintage products carry none), so `is_sealed` needs it.
pub const CATEGORY_POKEMON_JAPAN: i64 = 85;

/// A price object with no `subTypeName` — the default the SQLite importer applies.
pub const DEFAULT_SUB_TYPE: &str = "Normal";

/// `(price_type, JSON field)`, in the order the SQLite importer writes them. A null is not a
/// price and produces no row.
pub const PRICE_TYPES: [(&str, &str); 5] = [
    ("low", "lowPrice"),
    ("mid", "midPrice"),
    ("high", "highPrice"),
    ("market", "marketPrice"),
    ("directLow", "directLowPrice"),
];

/// The `results` array of one landed TCGCSV envelope.
pub fn results_of(body: &[u8], key: &str) -> Result<Vec<Value>> {
    let envelope: Value =
        serde_json::from_slice(body).with_context(|| format!("{key}: not JSON"))?;
    match envelope.get("results") {
        Some(Value::Array(a)) => Ok(a.clone()),
        _ => bail!("{key}: TCGCSV envelope missing 'results'"),
    }
}

fn product_id(entry: &Value) -> Result<i64> {
    let v = entry
        .get("productId")
        .with_context(|| format!("entry has no productId: {entry}"))?;
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .with_context(|| format!("productId {v} is not a number"))
}

fn price_of(entry: &Value, field: &str) -> Result<Option<f64>> {
    match entry.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => Ok(Some(
            v.as_f64()
                .with_context(|| format!("{field} {v} is not a number"))?,
        )),
    }
}

/// `results` into rows at `catalog.prices`'s grain.
pub fn price_rows(results: &[Value]) -> Result<Vec<PriceRow>> {
    let mut rows = Vec::new();
    for entry in results {
        let id = product_id(entry)?;
        let sub_type = entry
            .get("subTypeName")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_SUB_TYPE);
        for (price_type, field) in PRICE_TYPES {
            if let Some(price) = price_of(entry, field)? {
                rows.push(PriceRow {
                    tcgplayer_product_id: id,
                    sub_type_name: sub_type.to_string(),
                    price_type: price_type.to_string(),
                    price,
                });
            }
        }
    }
    Ok(rows)
}

/// `results` into `catalog.sealed_prices` rows, keyed by product, in first-seen order.
///
/// Keyed by product because the collapse is at the PRODUCT level: the first entry quoting a
/// sealed product wins **whole** (what `INSERT OR IGNORE` on `UNIQUE(product, observed_at)`
/// does in `tcgcsv.rs::import_prices`). Reducing per price type would take the market from one
/// sub-type's entry and the mid from another's — a pair nobody quoted.
pub fn sealed_rows_by_product(
    results: &[Value],
    sealed_ids: &HashSet<i64>,
) -> Result<Vec<(i64, Vec<SealedPriceRow>)>> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for entry in results {
        let id = product_id(entry)?;
        if !sealed_ids.contains(&id) || !seen.insert(id) {
            continue;
        }
        let mut rows = Vec::new();
        for (price_type, field) in PRICE_TYPES {
            if let Some(price) = price_of(entry, field)? {
                rows.push(SealedPriceRow {
                    tcgplayer_product_id: id,
                    price_type: price_type.to_string(),
                    price,
                });
            }
        }
        out.push((id, rows));
    }
    Ok(out)
}

/// Whether a landed TCGCSV product is sealed rather than a card. Category 3 reads an
/// extended-data `Number`, category 85 a `CardType`; a product that is not a card is sealed.
pub fn is_sealed(product: &Value, category_id: i64) -> bool {
    let field = if category_id == CATEGORY_POKEMON_JAPAN {
        "cardtype"
    } else {
        "number"
    };
    let is_card = product
        .get("extendedData")
        .and_then(Value::as_array)
        .is_some_and(|ext| {
            ext.iter().any(|e| {
                e.is_object()
                    && e.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|n| n.to_lowercase() == field)
            })
        });
    !is_card
}

/// The TCGplayer category a landed products URL was fetched from:
/// `{base}/{category}/{group}/products`. Read, never guessed, and refused rather than
/// defaulted — the default would be the English rule silently applied to Japanese cards.
pub fn category_of(url: &str, key: &str) -> Result<i64> {
    let path = url.split('?').next().unwrap_or("").trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 3 || *parts.last().unwrap() != "products" {
        bail!("{key}: {url:?} is not a TCGCSV products URL");
    }
    parts[parts.len() - 3]
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("{key}: no TCGplayer category in {url:?}"))
}

/// Which of a date's runs a rebuild reads (`raw.py::select_runs`).
///
/// The newest complete run wins, alone. With none, refuse — unless `allow_incomplete`, in
/// which case every run is read and the result is explicitly not a whole day.
pub fn select_runs(runs: &[Run], allow_incomplete: bool) -> Result<Vec<Run>> {
    if let Some(run) = runs.iter().rev().find(|r| r.manifest.complete) {
        return Ok(vec![run.clone()]);
    }
    if runs.is_empty() {
        bail!("no runs landed for that date");
    }
    if !allow_incomplete {
        let detail = runs
            .iter()
            .map(Run::describe)
            .collect::<Vec<_>>()
            .join("; ");
        bail!(
            "no complete run for that date — {detail}.\n\
             An incomplete run's parts are real bytes, but nothing in the landing zone can say \
             whether they add up to the whole day. Re-run the fetch, or pass \
             --allow-incomplete to build from what is there and have the table say so."
        );
    }
    Ok(runs.to_vec())
}

/// Every sealed product id in one date's products dataset, plus the run ULIDs read.
///
/// Refuses an empty partition: an empty sealed set is not "no sealed product was quoted", it
/// is "nothing here can be recognised as sealed", and it would write a table saying every
/// tenant's sealed holdings are worth nothing.
pub fn sealed_product_ids(
    runs: &[Run],
    ingest_date: &str,
    payload: &mut dyn FnMut(&PartRecord) -> Result<Vec<u8>>,
) -> Result<(HashSet<i64>, Vec<String>)> {
    let mut ids = HashSet::new();
    let mut parts = 0usize;
    for run in runs {
        for part in &run.manifest.parts {
            parts += 1;
            let category = category_of(&part.url, &part.key)?;
            for product in results_of(&payload(part)?, &part.key)? {
                if is_sealed(&product, category) {
                    ids.insert(product_id(&product)?);
                }
            }
        }
    }
    if parts == 0 {
        bail!(
            "ingest_date={ingest_date} landed no products parts, so nothing can say which \
             products are sealed. catalog.sealed_prices is not built from a guess: an empty \
             sealed table reads as 'every tenant owns no sealed product'."
        );
    }
    println!(
        "    {} sealed product id(s) from {parts} products part(s)",
        ids.len()
    );
    Ok((ids, runs.iter().map(|r| r.run_id.clone()).collect()))
}

/// Insertion-ordered map: a repeated key keeps its first position and takes the new value,
/// which is what `dict.update` does and what makes row order a parity matter.
struct Ordered<K, V> {
    index: HashMap<K, usize>,
    items: Vec<(K, V)>,
}

impl<K: std::hash::Hash + Eq + Clone, V> Ordered<K, V> {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            items: Vec::new(),
        }
    }
    fn set(&mut self, k: K, v: V) {
        match self.index.get(&k) {
            Some(&i) => self.items[i].1 = v,
            None => {
                self.index.insert(k.clone(), self.items.len());
                self.items.push((k, v));
            }
        }
    }
    fn set_default(&mut self, k: K, v: V) {
        if !self.index.contains_key(&k) {
            self.set(k, v);
        }
    }
}

type Grain = (i64, String, String);

fn grain(r: &PriceRow) -> Grain {
    (
        r.tcgplayer_product_id,
        r.sub_type_name.clone(),
        r.price_type.clone(),
    )
}

/// What one night's price parts reduce to.
pub struct Day {
    pub rows: Vec<PriceRow>,
    pub sealed: Vec<SealedPriceRow>,
    pub provenance: BTreeMap<String, String>,
}

/// Every price landed for one `ingest_date`, plus how it was assembled (`prices.py::read_date`).
///
/// Two collisions at the grain resolve in opposite directions on purpose: within one run the
/// FIRST wins (as `INSERT OR IGNORE` does), across runs, oldest first, the LATER wins (a
/// retry supersedes the attempt that failed). The sealed side follows both a level up.
pub fn read_date(
    runs: &[Run],
    ingest_date: &str,
    sealed_ids: &HashSet<i64>,
    payload: &mut dyn FnMut(&PartRecord) -> Result<Vec<u8>>,
) -> Result<Day> {
    let mut merged: Ordered<Grain, PriceRow> = Ordered::new();
    let mut sealed_merged: Ordered<i64, Vec<SealedPriceRow>> = Ordered::new();
    let mut parts = 0usize;
    let mut seen = 0usize;
    for run in runs {
        println!("    {}", run.describe());
        parts += run.manifest.parts.len();
        let mut within: Ordered<Grain, PriceRow> = Ordered::new();
        let mut within_sealed: Ordered<i64, Vec<SealedPriceRow>> = Ordered::new();
        for part in &run.manifest.parts {
            let results = results_of(&payload(part)?, &part.key)?;
            for row in price_rows(&results)? {
                seen += 1;
                within.set_default(grain(&row), row);
            }
            for (id, rows) in sealed_rows_by_product(&results, sealed_ids)? {
                within_sealed.set_default(id, rows);
            }
        }
        for (k, v) in within.items {
            merged.set(k, v);
        }
        for (k, v) in within_sealed.items {
            sealed_merged.set(k, v);
        }
    }
    if parts == 0 {
        bail!("ingest_date={ingest_date} landed no price parts");
    }
    if merged.items.len() != seen {
        println!(
            "    {} row(s) collapsed at the grain",
            seen - merged.items.len()
        );
    }
    let sealed = sealed_merged
        .items
        .into_iter()
        .flat_map(|(_, v)| v)
        .collect();

    let provenance = BTreeMap::from([
        ("pkdump.built-from".into(), "raw".into()),
        ("pkdump.raw-source".into(), "tcgcsv".into()),
        ("pkdump.raw-dataset".into(), "prices".into()),
        ("pkdump.raw-ingest-date".into(), ingest_date.into()),
        (
            "pkdump.raw-runs".into(),
            runs.iter()
                .map(|r| r.run_id.as_str())
                .collect::<Vec<_>>()
                .join(","),
        ),
        ("pkdump.raw-parts".into(), parts.to_string()),
        (
            "pkdump.raw-complete".into(),
            if runs.iter().all(|r| r.manifest.complete) {
                "true"
            } else {
                "false"
            }
            .into(),
        ),
    ]);
    Ok(Day {
        rows: merged.items.into_iter().map(|(_, v)| v).collect(),
        sealed,
        provenance,
    })
}

/// Parity with the Python lake. `tests/fixtures/prices_parity.json` is recorded FROM
/// `lake/src/pkdump_lake/{prices,raw}.py` by `record_prices_parity.py`; each test feeds this
/// port the recorded inputs and requires the recorded outputs, so a divergence is a diff
/// rather than a judgement.
#[cfg(test)]
mod parity {
    use super::*;
    use dumpster_kit_lake::{Manifest, PartRecord};
    use serde_json::json;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/fixtures/prices_parity.json")).unwrap()
    }

    fn rows_json(rows: &[PriceRow]) -> Value {
        Value::Array(
            rows.iter()
                .map(|r| {
                    json!({"price": r.price, "price_type": r.price_type,
                           "sub_type_name": r.sub_type_name,
                           "tcgplayer_product_id": r.tcgplayer_product_id})
                })
                .collect(),
        )
    }

    fn sealed_json(rows: &[SealedPriceRow]) -> Value {
        Value::Array(
            rows.iter()
                .map(|r| {
                    json!({"price": r.price, "price_type": r.price_type,
                           "tcgplayer_product_id": r.tcgplayer_product_id})
                })
                .collect(),
        )
    }

    fn run_of(r: &Value) -> Run {
        let id = r["run_id"].as_str().unwrap();
        let mut m = Manifest::new(
            dumpster_kit_lake::Source::TCGCSV,
            dumpster_kit_lake::Dataset::PRICES,
            "2026-08-11",
            id,
            "",
        );
        m.complete = r["complete"].as_bool().unwrap();
        m.parts = r["parts"]
            .as_array()
            .map(|ps| {
                ps.iter()
                    .map(|p| PartRecord {
                        key: p["key"].as_str().unwrap().into(),
                        url: p["url"].as_str().unwrap().into(),
                        status: 200,
                        bytes: 0,
                        sha256: String::new(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Run {
            run_id: id.into(),
            prefix: format!("raw/{id}/"),
            manifest: m,
        }
    }

    #[test]
    fn price_rows_match_the_python() {
        for case in fixture()["price_cases"].as_array().unwrap() {
            let results = results_of(case["body"].as_str().unwrap().as_bytes(), "k").unwrap();
            assert_eq!(
                rows_json(&price_rows(&results).unwrap()),
                case["expected"],
                "{}",
                case["name"]
            );
        }
    }

    #[test]
    fn sealed_rows_match_the_python_including_first_entry_wins_whole() {
        for case in fixture()["sealed_cases"].as_array().unwrap() {
            let ids: HashSet<i64> = case["sealed_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect();
            let results = results_of(case["body"].as_str().unwrap().as_bytes(), "k").unwrap();
            let got: Vec<Value> = sealed_rows_by_product(&results, &ids)
                .unwrap()
                .into_iter()
                .map(|(id, rows)| json!([id, sealed_json(&rows)]))
                .collect();
            assert_eq!(Value::Array(got), case["expected"]);
        }
    }

    #[test]
    fn the_sealed_discriminator_matches_the_python() {
        for case in fixture()["is_sealed"].as_array().unwrap() {
            assert_eq!(
                is_sealed(&case["product"], case["category"].as_i64().unwrap()),
                case["expected"].as_bool().unwrap(),
                "{}",
                case["product"]
            );
        }
    }

    #[test]
    fn the_category_is_read_from_the_url_or_refused() {
        for case in fixture()["category_of"].as_array().unwrap() {
            let got = category_of(case["url"].as_str().unwrap(), "k").ok();
            assert_eq!(got, case["expected"].as_i64(), "{}", case["url"]);
        }
    }

    #[test]
    fn run_selection_matches_the_python() {
        for case in fixture()["select_runs"].as_array().unwrap() {
            let runs: Vec<Run> = case["runs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| run_of(&json!({"run_id": r["run_id"], "complete": r["complete"]})))
                .collect();
            let got = select_runs(&runs, case["allow_incomplete"].as_bool().unwrap())
                .ok()
                .map(|rs| rs.into_iter().map(|r| r.run_id).collect::<Vec<_>>());
            let want = case["chosen"].as_array().map(|a| {
                a.iter()
                    .map(|v| v.as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            });
            assert_eq!(got, want, "{case}");
        }
    }

    #[test]
    fn read_date_matches_the_python_rows_order_and_provenance() {
        for case in fixture()["read_date"].as_array().unwrap() {
            let runs_json = case["runs"].as_array().unwrap();
            let bodies: HashMap<String, Vec<u8>> = runs_json
                .iter()
                .flat_map(|r| r["parts"].as_array().unwrap())
                .map(|p| {
                    (
                        p["key"].as_str().unwrap().to_string(),
                        p["body"].as_str().unwrap().as_bytes().to_vec(),
                    )
                })
                .collect();
            let all: Vec<Run> = runs_json.iter().map(run_of).collect();
            let chosen = select_runs(&all, case["allow_incomplete"].as_bool().unwrap()).unwrap();
            let ids: HashSet<i64> = case["sealed_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap())
                .collect();
            let mut fetch = |p: &PartRecord| -> Result<Vec<u8>> { Ok(bodies[&p.key].clone()) };
            let day = read_date(&chosen, "2026-08-11", &ids, &mut fetch).unwrap();
            assert_eq!(rows_json(&day.rows), case["rows"], "rows");
            assert_eq!(sealed_json(&day.sealed), case["sealed"], "sealed");
            let want: BTreeMap<String, String> =
                serde_json::from_value(case["provenance"].clone()).unwrap();
            assert_eq!(day.provenance, want, "provenance");
        }
    }
}
