//! Importer for TCGCSV (`tcgcsv.com`) — the daily TCGplayer bulk dump.
//!
//! Provides set ("group") metadata, the sealed-product catalog, and spot
//! prices (RESEARCH.md §2.5). categoryId 3 is Pokémon. No auth, no rate
//! limit. PokeDumpster snapshots prices daily into a time series.

use std::collections::HashSet;
use std::time::Duration;

use rusqlite::Connection;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{IngestError, Result};

const BASE_URL: &str = "https://tcgcsv.com/tcgplayer/3";

/// A TCGplayer "group" — roughly a set.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TcgGroup {
    pub group_id: i64,
    pub name: String,
    pub abbreviation: Option<String>,
    pub published_on: Option<String>,
}

/// One `extendedData` entry on a product. Single cards carry a `Number`
/// entry here; sealed products do not.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtendedDatum {
    pub name: String,
    #[serde(default)]
    pub value: String,
}

/// A TCGplayer product — either a single card or a sealed product.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TcgProduct {
    pub product_id: i64,
    pub group_id: i64,
    pub name: String,
    pub image_url: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub extended_data: Vec<ExtendedDatum>,
}

/// A spot price for one product + printing sub-type.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TcgPrice {
    pub product_id: i64,
    pub sub_type_name: Option<String>,
    pub low_price: Option<f64>,
    pub mid_price: Option<f64>,
    pub high_price: Option<f64>,
    pub market_price: Option<f64>,
    pub direct_low_price: Option<f64>,
}

/// Extract and deserialize the `results` array from a TCGCSV envelope.
fn parse_results<T: DeserializeOwned>(envelope: &Value) -> Result<Vec<T>> {
    let arr = envelope
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| IngestError::BadResponse("TCGCSV envelope missing 'results'".into()))?;
    arr.iter()
        .map(|v| serde_json::from_value(v.clone()).map_err(IngestError::from))
        .collect()
}

/// Whether a product is a single card (it carries a `Number` extendedData
/// entry). Everything else is treated as a sealed product.
pub fn is_single_card(product: &TcgProduct) -> bool {
    product
        .extended_data
        .iter()
        .any(|e| e.name.eq_ignore_ascii_case("Number"))
}

/// Classify a sealed product into a coarse category from its name.
pub fn classify_sealed(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("elite trainer box") || n.contains("etb") {
        "elite_trainer_box"
    } else if n.contains("booster box") {
        "booster_box"
    } else if n.contains("bundle") {
        "bundle"
    } else if n.contains("tin") {
        "tin"
    } else if n.contains("blister") || n.contains("sleeved booster") {
        "blister"
    } else if n.contains("premium") || n.contains("collection") {
        "collection_box"
    } else if n.contains("case") {
        "case"
    } else if n.contains("booster pack") || n.ends_with(" pack") {
        "booster_pack"
    } else {
        "other"
    }
}

/// Import groups into `tcgplayer_groups`, best-effort bridging each to a set
/// via `abbreviation` ↔ `sets.ptcgo_code`. Returns the number of groups.
pub fn import_groups(conn: &mut Connection, groups: &[TcgGroup], now: &str) -> Result<usize> {
    let tx = conn.transaction()?;
    for g in groups {
        tx.execute(
            "INSERT INTO tcgplayer_groups
               (group_id, set_code, name, abbreviation, published_on, fetched_at)
             VALUES (?1,
                     (SELECT set_code FROM sets WHERE ptcgo_code = ?4 COLLATE NOCASE),
                     ?2, ?4, ?3, ?5)
             ON CONFLICT(group_id) DO UPDATE SET
               set_code     = excluded.set_code,
               name         = excluded.name,
               abbreviation = excluded.abbreviation,
               published_on = excluded.published_on,
               fetched_at   = excluded.fetched_at",
            rusqlite::params![g.group_id, g.name, g.published_on, g.abbreviation, now],
        )?;
        // Reciprocal link so the set knows its TCGCSV group.
        if g.abbreviation.is_some() {
            tx.execute(
                "UPDATE sets SET tcgcsv_group_id = ?1 \
                 WHERE ptcgo_code = ?2 COLLATE NOCASE",
                rusqlite::params![g.group_id, g.abbreviation],
            )?;
        }
    }
    tx.commit()?;
    Ok(groups.len())
}

/// Import the sealed products from a group's product list (single cards are
/// skipped — they are catalogued from pokemon-tcg-data instead).
pub fn import_sealed_products(
    conn: &mut Connection,
    products: &[TcgProduct],
    now: &str,
) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut n = 0;
    for p in products {
        if is_single_card(p) {
            continue;
        }
        tx.execute(
            "INSERT INTO sealed_products
               (product_id, set_code, name, category, image_url, tcgplayer_url, fetched_at)
             VALUES (?1,
                     (SELECT set_code FROM tcgplayer_groups WHERE group_id = ?2),
                     ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(product_id) DO UPDATE SET
               set_code      = excluded.set_code,
               name          = excluded.name,
               category      = excluded.category,
               image_url     = excluded.image_url,
               tcgplayer_url = excluded.tcgplayer_url,
               fetched_at    = excluded.fetched_at",
            rusqlite::params![
                p.product_id,
                p.group_id,
                p.name,
                classify_sealed(&p.name),
                p.image_url,
                p.url,
                now,
            ],
        )?;
        n += 1;
    }
    tx.commit()?;
    Ok(n)
}

/// Snapshot prices. Card-product prices land in the narrow `prices` time
/// series (one row per non-null price type); sealed-product prices land in
/// `sealed_prices`. Idempotent for a given `observed_at` via INSERT OR IGNORE.
pub fn import_prices(conn: &mut Connection, prices: &[TcgPrice], observed_at: &str) -> Result<usize> {
    let sealed: HashSet<i64> = {
        let mut stmt = conn.prepare("SELECT product_id FROM sealed_products")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let tx = conn.transaction()?;
    let mut n = 0;
    for p in prices {
        if sealed.contains(&p.product_id) {
            tx.execute(
                "INSERT OR IGNORE INTO sealed_prices
                   (tcgplayer_product_id, low_price, mid_price, high_price,
                    market_price, direct_low_price, observed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    p.product_id,
                    p.low_price,
                    p.mid_price,
                    p.high_price,
                    p.market_price,
                    p.direct_low_price,
                    observed_at,
                ],
            )?;
            n += 1;
        } else {
            let sub = p.sub_type_name.as_deref().unwrap_or("Normal");
            for (price_type, value) in [
                ("low", p.low_price),
                ("mid", p.mid_price),
                ("high", p.high_price),
                ("market", p.market_price),
                ("directLow", p.direct_low_price),
            ] {
                if let Some(v) = value {
                    tx.execute(
                        "INSERT OR IGNORE INTO prices
                           (tcgplayer_product_id, sub_type_name, source,
                            price_type, price, observed_at)
                         VALUES (?1, ?2, 'tcgplayer', ?3, ?4, ?5)",
                        rusqlite::params![p.product_id, sub, price_type, v, observed_at],
                    )?;
                    n += 1;
                }
            }
        }
    }
    tx.commit()?;
    Ok(n)
}

/// A blocking client for the TCGCSV endpoints.
pub struct TcgcsvClient {
    http: reqwest::blocking::Client,
}

impl TcgcsvClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .user_agent("pokedumpster/0.1 (+cache-population)")
                .timeout(Duration::from_secs(60))
                .build()?,
        })
    }

    fn get(&self, path: &str) -> Result<Value> {
        std::thread::sleep(Duration::from_millis(50));
        Ok(self
            .http
            .get(format!("{BASE_URL}{path}"))
            .send()?
            .error_for_status()?
            .json()?)
    }

    /// Every Pokémon group (set).
    pub fn fetch_groups(&self) -> Result<Vec<TcgGroup>> {
        parse_results(&self.get("/groups")?)
    }

    /// Every product (cards + sealed) in a group.
    pub fn fetch_products(&self, group_id: i64) -> Result<Vec<TcgProduct>> {
        parse_results(&self.get(&format!("/{group_id}/products"))?)
    }

    /// Every spot price in a group.
    pub fn fetch_prices(&self, group_id: i64) -> Result<Vec<TcgPrice>> {
        parse_results(&self.get(&format!("/{group_id}/prices"))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn parses_groups_envelope() {
        let env: Value = serde_json::from_str(
            r#"{"success":true,"results":[
                 {"groupId":23237,"name":"151","abbreviation":"MEW",
                  "publishedOn":"2023-09-22T00:00:00"}]}"#,
        )
        .unwrap();
        let groups: Vec<TcgGroup> = parse_results(&env).unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].group_id, 23237);
        assert_eq!(groups[0].abbreviation.as_deref(), Some("MEW"));
    }

    #[test]
    fn single_card_vs_sealed_classification() {
        let card = TcgProduct {
            product_id: 1,
            group_id: 1,
            name: "Charizard ex".into(),
            image_url: None,
            url: None,
            extended_data: vec![ExtendedDatum {
                name: "Number".into(),
                value: "6".into(),
            }],
        };
        let sealed = TcgProduct {
            product_id: 2,
            group_id: 1,
            name: "151 Elite Trainer Box".into(),
            image_url: None,
            url: None,
            extended_data: vec![],
        };
        assert!(is_single_card(&card));
        assert!(!is_single_card(&sealed));
        assert_eq!(classify_sealed("151 Elite Trainer Box"), "elite_trainer_box");
        assert_eq!(classify_sealed("Surging Sparks Booster Box"), "booster_box");
        assert_eq!(classify_sealed("Mystery Item"), "other");
    }

    #[test]
    fn import_groups_bridges_to_set() {
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('sv3pt5','MEW','151','Scarlet & Violet')",
            [],
        )
        .unwrap();
        let groups = vec![TcgGroup {
            group_id: 23237,
            name: "151".into(),
            abbreviation: Some("MEW".into()),
            published_on: None,
        }];
        import_groups(&mut conn, &groups, "2026-05-18").unwrap();

        let set_code: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 23237",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(set_code.as_deref(), Some("sv3pt5"));

        let gid: Option<i64> = conn
            .query_row(
                "SELECT tcgcsv_group_id FROM sets WHERE set_code = 'sv3pt5'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gid, Some(23237));
    }

    #[test]
    fn imports_only_sealed_products() {
        let (_d, mut conn) = shared_db();
        let products = vec![
            TcgProduct {
                product_id: 100,
                group_id: 1,
                name: "Pikachu".into(),
                image_url: None,
                url: None,
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "25".into(),
                }],
            },
            TcgProduct {
                product_id: 200,
                group_id: 1,
                name: "151 Booster Box".into(),
                image_url: None,
                url: None,
                extended_data: vec![],
            },
        ];
        let n = import_sealed_products(&mut conn, &products, "2026-05-18").unwrap();
        assert_eq!(n, 1);
        let count: i64 = conn
            .query_row("SELECT count(*) FROM sealed_products", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let cat: String = conn
            .query_row(
                "SELECT category FROM sealed_products WHERE product_id = 200",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cat, "booster_box");
    }

    #[test]
    fn import_prices_routes_card_and_sealed() {
        let (_d, mut conn) = shared_db();
        // Mark product 200 as sealed.
        let sealed = vec![TcgProduct {
            product_id: 200,
            group_id: 1,
            name: "151 Booster Box".into(),
            image_url: None,
            url: None,
            extended_data: vec![],
        }];
        import_sealed_products(&mut conn, &sealed, "2026-05-18").unwrap();

        let prices = vec![
            TcgPrice {
                product_id: 100, // card
                sub_type_name: Some("Holofoil".into()),
                low_price: Some(4.0),
                mid_price: Some(10.0),
                high_price: Some(80.0),
                market_price: Some(10.5),
                direct_low_price: None,
            },
            TcgPrice {
                product_id: 200, // sealed
                sub_type_name: None,
                low_price: Some(140.0),
                mid_price: Some(160.0),
                high_price: Some(220.0),
                market_price: Some(155.0),
                direct_low_price: None,
            },
        ];
        import_prices(&mut conn, &prices, "2026-05-18").unwrap();

        // Card price: 4 rows (low/mid/high/market — directLow was null).
        let card_rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM prices WHERE tcgplayer_product_id = 100",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(card_rows, 4);

        // Sealed price routed to sealed_prices.
        let sealed_rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM sealed_prices WHERE tcgplayer_product_id = 200",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sealed_rows, 1);

        // Idempotent for the same observed_at.
        import_prices(&mut conn, &prices, "2026-05-18").unwrap();
        let card_rows2: i64 = conn
            .query_row(
                "SELECT count(*) FROM prices WHERE tcgplayer_product_id = 100",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(card_rows2, 4, "same-day re-snapshot must not duplicate");
    }
}
