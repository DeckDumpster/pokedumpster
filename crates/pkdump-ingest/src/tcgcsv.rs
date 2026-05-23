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

/// Normalize a collector number so the catalog and TCGCSV agree.
///
/// pokemontcg.io stores bare numbers (`"6"`, `"H1"`, `"SWSH001"`) while
/// TCGCSV's `extendedData` "Number" carries the printed form, which for
/// modern sets is zero-padded and suffixed with the set total
/// (`"006/165"`, `"H01/H32"`). Linking by the raw strings matches almost
/// nothing. Normalization drops the `/total` suffix and collapses every
/// run of digits to its integer value, so both sides reduce to the same
/// token: `"006/165"` → `"6"`, `"H01/H32"` → `"h1"`, `"SWSH001"` →
/// `"swsh1"`. Applied identically to both sides it is order-preserving and
/// idempotent.
pub fn normalize_collector_number(raw: &str) -> String {
    let first = raw
        .split('/')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase();
    let mut out = String::with_capacity(first.len());
    let mut digits = String::new();
    for ch in first.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            if !digits.is_empty() {
                out.push_str(&digits.parse::<u64>().unwrap_or(0).to_string());
                digits.clear();
            }
            out.push(ch);
        }
    }
    if !digits.is_empty() {
        out.push_str(&digits.parse::<u64>().unwrap_or(0).to_string());
    }
    out
}

/// Normalize a set name so pokemontcg.io sets and TCGCSV groups can be
/// bridged on name when no `ptcgo_code`/`abbreviation` pair lines them up.
///
/// TCGCSV group names carry an era prefix the catalog name lacks
/// (`"SWSH08: Fusion Strike"`, `"SM - Cosmic Eclipse"`,
/// `"SWSH: Crown Zenith: Galarian Gallery"`); pokemontcg.io stores just
/// `"Fusion Strike"`. The prefix is stripped (it can repeat once), `&` is
/// unified with `and`, and everything reduces to lowercase alphanumeric
/// words. `"SWSH08: Fusion Strike"` and `"Fusion Strike"` both become
/// `"fusion strike"`.
pub fn normalize_set_name(raw: &str) -> String {
    let mut s = raw.to_ascii_lowercase();
    // Strip a leading era code prefix like "swsh08:" / "sm -" / "sv:".
    // It can appear twice ("swsh: crown zenith: galarian gallery").
    for _ in 0..2 {
        if let Some(pos) = s.find([':', '-']) {
            let head = &s[..pos];
            let head_ok = !head.is_empty()
                && head.trim().len() <= 6
                && head
                    .trim()
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace())
                && head.trim().chars().any(|c| c.is_ascii_alphabetic());
            if head_ok {
                s = s[pos + 1..].to_string();
                continue;
            }
        }
        break;
    }
    s = s.replace('&', " and ");
    let words: Vec<&str> = s
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.join(" ")
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

/// Resolve which catalog set each TCGCSV group bridges to.
///
/// A group is bridged to a set in two tiers, the first that yields a
/// not-yet-claimed set winning:
///   1. `group.abbreviation` ↔ `sets.ptcgo_code` (case-insensitive),
///   2. `normalize_set_name(group.name)` ↔ `normalize_set_name(set.name)`.
///
/// Each set is claimed by at most one group and each group bridges to at
/// most one set, so `sets.tcgcsv_group_id` stays UNIQUE. `ptcgo_code` is
/// not unique (promo codes recur, many are NULL) and TCGCSV reuses a name
/// across the odd group, so any tier may offer the same set to several
/// groups — the first group (by id, deterministic) takes it. Groups are
/// processed in id order so the assignment is stable across re-runs.
fn resolve_group_set_links(
    conn: &Connection,
    groups: &[TcgGroup],
) -> Result<std::collections::HashMap<i64, String>> {
    // ptcgo_code (lowercased) -> [set_code, ...] in release order.
    let mut by_ptcgo: std::collections::HashMap<String, Vec<String>> = Default::default();
    // normalized name -> [set_code, ...]; a normalized name maps to one set
    // in real data, but a Vec keeps the claim logic uniform.
    let mut by_name: std::collections::HashMap<String, Vec<String>> = Default::default();
    {
        let mut stmt = conn.prepare(
            "SELECT set_code, ptcgo_code, name FROM sets \
             ORDER BY release_date, set_code",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (set_code, ptcgo, name) = row?;
            if let Some(code) = ptcgo.filter(|c| !c.is_empty()) {
                by_ptcgo
                    .entry(code.to_ascii_lowercase())
                    .or_default()
                    .push(set_code.clone());
            }
            by_name
                .entry(normalize_set_name(&name))
                .or_default()
                .push(set_code);
        }
    }

    let mut sorted: Vec<&TcgGroup> = groups.iter().collect();
    sorted.sort_by_key(|g| g.group_id);

    let mut claimed: HashSet<String> = HashSet::new();
    let mut links: std::collections::HashMap<i64, String> = Default::default();
    for g in sorted {
        // Tier 1: ptcgo_code / abbreviation.
        let mut chosen: Option<String> = None;
        if let Some(abbr) = g.abbreviation.as_deref().filter(|a| !a.is_empty())
            && let Some(candidates) = by_ptcgo.get(&abbr.to_ascii_lowercase())
        {
            chosen = candidates.iter().find(|c| !claimed.contains(*c)).cloned();
        }
        // Tier 2: normalized set name.
        if chosen.is_none()
            && let Some(candidates) = by_name.get(&normalize_set_name(&g.name))
        {
            chosen = candidates.iter().find(|c| !claimed.contains(*c)).cloned();
        }
        if let Some(set_code) = chosen {
            claimed.insert(set_code.clone());
            links.insert(g.group_id, set_code);
        }
    }
    Ok(links)
}

/// Import groups into `tcgplayer_groups`, bridging each to a catalog set via
/// [`resolve_group_set_links`]. Both `tcgplayer_groups.set_code` and the
/// reciprocal `sets.tcgcsv_group_id` are written from that single decision
/// so the two links never disagree. Returns the number of groups.
pub fn import_groups(conn: &mut Connection, groups: &[TcgGroup], now: &str) -> Result<usize> {
    let links = resolve_group_set_links(conn, groups)?;
    let tx = conn.transaction()?;
    // Re-derive every link from scratch so `data refresh` re-runs converge
    // on the same state regardless of prior contents.
    tx.execute("UPDATE sets SET tcgcsv_group_id = NULL", [])?;
    for g in groups {
        let set_code = links.get(&g.group_id);
        tx.execute(
            "INSERT INTO tcgplayer_groups
               (group_id, set_code, name, abbreviation, published_on, fetched_at)
             VALUES (?1, ?6, ?2, ?4, ?3, ?5)
             ON CONFLICT(group_id) DO UPDATE SET
               set_code     = excluded.set_code,
               name         = excluded.name,
               abbreviation = excluded.abbreviation,
               published_on = excluded.published_on,
               fetched_at   = excluded.fetched_at",
            rusqlite::params![
                g.group_id,
                g.name,
                g.published_on,
                g.abbreviation,
                now,
                set_code
            ],
        )?;
        if let Some(set_code) = set_code {
            tx.execute(
                "UPDATE sets SET tcgcsv_group_id = ?1 WHERE set_code = ?2",
                rusqlite::params![g.group_id, set_code],
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

/// Variants priced as sub_types of a single base TCGplayer product (Normal,
/// Holofoil, Reverse Holofoil, and the period-specific foil treatments). A
/// base product's UPDATE may legitimately touch any of these — they share
/// `tcgplayer_product_id` and distinguish their price via VARIANT_PRICE_SUBTYPE.
/// Pattern variants (Poké Ball, Master Ball, stamps) live as their own
/// TCGplayer products and must not be overwritten by a base-product UPDATE.
const BASE_PRODUCT_VARIANTS: &[&str] = &[
    "normal",
    "holo",
    "reverse_holo",
    "first_ed_holo",
    "first_ed_normal",
    "unlimited_holo",
    "cosmos_holo",
];

/// Derive the printing variant carried by a TCGplayer product name. Returns
/// `None` for the base product (covers normal / reverse_holo / holo via
/// sub_types on one product). Pattern products get their own product id and
/// link to a specific printing_id.
///
/// Token coverage (current as of 2026-05): ball patterns (Master, Quick,
/// Dusk, Love, Friend, Poké) and the Ascended Heroes "Energy Symbol
/// Pattern" + "Team Rocket" treatments. Match order is significant:
/// more-specific tokens first so e.g. "Master Ball" doesn't fall through
/// to a generic "Ball" rule.
pub fn variant_from_product_name(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if lower.contains("master ball") {
        Some("masterball_rh")
    } else if lower.contains("quick ball") {
        Some("quickball_rh")
    } else if lower.contains("dusk ball") {
        Some("duskball_rh")
    } else if lower.contains("love ball") {
        Some("loveball_rh")
    } else if lower.contains("friend ball") {
        Some("friendball_rh")
    } else if lower.contains("poke ball") || lower.contains("poké ball") {
        Some("pokeball_rh")
    } else if lower.contains("energy symbol") {
        Some("energy_symbol_rh")
    } else if lower.contains("team rocket") {
        Some("team_rocket_rh")
    } else {
        None
    }
}

/// Link single-card TCGplayer products to catalog printings. Resolves the
/// card by the group's bridged set + product collector `Number`, then routes
/// the link by product-name pattern: a base product (Bulbasaur, "Swadloon")
/// tags every base-foil printing of that card so price subqueries pick up
/// the right sub_type per variant. A pattern product ("Swadloon - Master
/// Ball Pattern") tags only its specific printing_id so it doesn't
/// overwrite the base link and each pattern keeps its own pricing series.
///
/// The catalog and TCGCSV disagree on the printed form of a collector
/// number, so the match runs on [`normalize_collector_number`] applied to
/// both sides rather than a raw string compare. Best-effort — products
/// whose normalized number doesn't match a catalog card, or whose derived
/// printing variant isn't in the catalog, are skipped silently.
/// Returns the number of products linked.
pub fn link_card_printings(conn: &mut Connection, products: &[TcgProduct]) -> Result<usize> {
    // group_id -> (normalized number -> card_id) for the bridged set.
    // A normalized number can be shared by several cards (artwork variants
    // reuse a printed number); each is linkable to its own product, so the
    // table is keyed per group and the last card wins only on a true tie.
    use std::collections::HashMap;
    let mut by_group: HashMap<i64, HashMap<String, String>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT s.tcgcsv_group_id, c.number, c.card_id \
             FROM cards c \
             JOIN sets s ON c.set_code = s.set_code \
             WHERE s.tcgcsv_group_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (group_id, number, card_id) = row?;
            by_group
                .entry(group_id)
                .or_default()
                .insert(normalize_collector_number(&number), card_id);
        }
    }

    // Sqlite needs a literal IN-list; build it from the constant.
    let base_in = BASE_PRODUCT_VARIANTS
        .iter()
        .map(|v| format!("'{v}'"))
        .collect::<Vec<_>>()
        .join(",");
    let base_sql = format!(
        "UPDATE printings SET tcgplayer_product_id = ?1 WHERE card_id = ?2 AND variant IN ({base_in})"
    );

    let tx = conn.transaction()?;
    let mut linked = 0;
    for product in products {
        if !is_single_card(product) {
            continue;
        }
        let number = product
            .extended_data
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("Number"))
            .map(|e| e.value.as_str())
            .unwrap_or_default();
        if number.trim().is_empty() {
            continue;
        }
        let Some(card_id) = by_group
            .get(&product.group_id)
            .and_then(|m| m.get(&normalize_collector_number(number)))
        else {
            continue;
        };
        let updated = match variant_from_product_name(&product.name) {
            Some(pattern) => {
                // UPSERT — if the variant-expansion overlay didn't already
                // create the printing (most modern pattern variants aren't
                // in our overlay), the TCGCSV product is itself the proof
                // the printing exists in the real world, so we materialize
                // it here and link it. An existing soft-deprecated row
                // (from a prior expansion that dropped the variant) is
                // un-deprecated so the printing stays live across runs.
                let printing_id = format!("{card_id}-{pattern}");
                tx.execute(
                    "INSERT INTO printings (printing_id, card_id, variant, language, tcgplayer_product_id) \
                     VALUES (?1, ?2, ?3, 'en', ?4) \
                     ON CONFLICT(printing_id) DO UPDATE SET \
                       deprecated_at = NULL, \
                       tcgplayer_product_id = excluded.tcgplayer_product_id",
                    rusqlite::params![printing_id, card_id, pattern, product.product_id],
                )?
            }
            None => tx.execute(&base_sql, rusqlite::params![product.product_id, card_id])?,
        };
        if updated > 0 {
            linked += 1;
        }
    }
    tx.commit()?;
    Ok(linked)
}

/// Snapshot prices. Card-product prices land in the narrow `prices` time
/// series (one row per non-null price type); sealed-product prices land in
/// `sealed_prices`. Idempotent for a given `observed_at` via INSERT OR IGNORE.
pub fn import_prices(
    conn: &mut Connection,
    prices: &[TcgPrice],
    observed_at: &str,
) -> Result<usize> {
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
        assert_eq!(
            classify_sealed("151 Elite Trainer Box"),
            "elite_trainer_box"
        );
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
    fn import_groups_survives_shared_ptcgo_code() {
        // Two sets share a ptcgo_code (real promo codes recur). Two groups
        // carry that code. The import must not violate UNIQUE(tcgcsv_group_id)
        // — each group links a single distinct set.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('promoA','PR','Promos A','Promo','2021/01/01'), \
                    ('promoB','PR','Promos B','Promo','2022/01/01')",
            [],
        )
        .unwrap();
        let groups = vec![
            TcgGroup {
                group_id: 1001,
                name: "Promos A".into(),
                abbreviation: Some("PR".into()),
                published_on: None,
            },
            TcgGroup {
                group_id: 1002,
                name: "Promos B".into(),
                abbreviation: Some("PR".into()),
                published_on: None,
            },
        ];
        // Must not panic on the UNIQUE constraint.
        import_groups(&mut conn, &groups, "2026-05-19").unwrap();
        // Re-running must stay idempotent (no second collision).
        import_groups(&mut conn, &groups, "2026-05-19").unwrap();

        let linked: i64 = conn
            .query_row(
                "SELECT count(*) FROM sets WHERE tcgcsv_group_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 2, "each group linked one distinct set");
        let distinct: i64 = conn
            .query_row(
                "SELECT count(DISTINCT tcgcsv_group_id) FROM sets \
                 WHERE tcgcsv_group_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(distinct, 2, "the two sets hold distinct group ids");
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

    #[test]
    fn links_card_printings_to_products() {
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('sv3pt5', 'MEW', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
             VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
            [],
        )
        .unwrap();
        for v in ["normal", "reverse_holo"] {
            conn.execute(
                "INSERT INTO printings (printing_id, card_id, variant) VALUES (?1, 'sv3pt5-6', ?2)",
                rusqlite::params![format!("sv3pt5-6-{v}"), v],
            )
            .unwrap();
        }
        // Bridge a TCGCSV group to the set.
        import_groups(
            &mut conn,
            &[TcgGroup {
                group_id: 23237,
                name: "151".into(),
                abbreviation: Some("MEW".into()),
                published_on: None,
            }],
            "2026-05-18",
        )
        .unwrap();

        let products = vec![TcgProduct {
            product_id: 5006,
            group_id: 23237,
            name: "Charizard ex".into(),
            image_url: None,
            url: None,
            extended_data: vec![ExtendedDatum {
                name: "Number".into(),
                value: "6".into(),
            }],
        }];
        let linked = link_card_printings(&mut conn, &products).unwrap();
        assert_eq!(linked, 1);

        // Every printing of the card now carries the product id.
        let pid: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings WHERE printing_id = 'sv3pt5-6-reverse_holo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pid, 5006);
    }

    #[test]
    fn variant_from_product_name_picks_pattern_codes() {
        assert_eq!(variant_from_product_name("Bulbasaur"), None);
        assert_eq!(
            variant_from_product_name("Bulbasaur - Poke Ball Pattern"),
            Some("pokeball_rh")
        );
        assert_eq!(
            variant_from_product_name("Bulbasaur (Poké Ball Pattern)"),
            Some("pokeball_rh")
        );
        assert_eq!(
            variant_from_product_name("Swadloon - Master Ball Pattern"),
            Some("masterball_rh")
        );
        // Case-insensitive.
        assert_eq!(
            variant_from_product_name("VICTINI MASTER BALL PATTERN"),
            Some("masterball_rh")
        );
    }

    #[test]
    fn link_card_printings_routes_pattern_to_specific_printing() {
        // Real-world: 151 Bulbasaur has Normal + Reverse Holofoil sub_types
        // on a base product (502552), plus separate Poke Ball / Master Ball
        // products. Each pattern must get its own product id so its price
        // chart doesn't shadow the base's.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('sv3pt5', 'MEW', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
             VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur')",
            [],
        )
        .unwrap();
        for v in ["normal", "reverse_holo", "pokeball_rh", "masterball_rh"] {
            conn.execute(
                "INSERT INTO printings (printing_id, card_id, variant) VALUES (?1, 'sv3pt5-1', ?2)",
                rusqlite::params![format!("sv3pt5-1-{v}"), v],
            )
            .unwrap();
        }
        import_groups(
            &mut conn,
            &[TcgGroup {
                group_id: 23237,
                name: "151".into(),
                abbreviation: Some("MEW".into()),
                published_on: None,
            }],
            "2026-05-18",
        )
        .unwrap();

        let number_attr = vec![ExtendedDatum {
            name: "Number".into(),
            value: "1".into(),
        }];
        let products = vec![
            TcgProduct {
                product_id: 502552,
                group_id: 23237,
                name: "Bulbasaur".into(),
                image_url: None,
                url: None,
                extended_data: number_attr.clone(),
            },
            TcgProduct {
                product_id: 502553,
                group_id: 23237,
                name: "Bulbasaur - Poke Ball Pattern".into(),
                image_url: None,
                url: None,
                extended_data: number_attr.clone(),
            },
            TcgProduct {
                product_id: 502554,
                group_id: 23237,
                name: "Bulbasaur - Master Ball Pattern".into(),
                image_url: None,
                url: None,
                extended_data: number_attr,
            },
        ];
        link_card_printings(&mut conn, &products).unwrap();

        let lookup = |pid: &str| -> i64 {
            conn.query_row(
                "SELECT tcgplayer_product_id FROM printings WHERE printing_id = ?1",
                [pid],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(lookup("sv3pt5-1-normal"), 502552, "base product on normal");
        assert_eq!(
            lookup("sv3pt5-1-reverse_holo"),
            502552,
            "base product also on reverse_holo (shared via sub_type)"
        );
        assert_eq!(
            lookup("sv3pt5-1-pokeball_rh"),
            502553,
            "Poke Ball product on its own printing"
        );
        assert_eq!(
            lookup("sv3pt5-1-masterball_rh"),
            502554,
            "Master Ball product on its own printing"
        );
    }

    #[test]
    fn link_card_printings_auto_creates_pattern_printing_when_absent() {
        // TCGCSV is authoritative for which pattern products exist. If our
        // variant overlay didn't pre-create the matching printing (most
        // modern pattern variants — Energy Symbol, the various Balls
        // beyond Poké/Master — have no overlay rule), the linker
        // materializes the printing via UPSERT and links it. This is what
        // makes WHT/BLK/ASC pattern variants surface without a hand-curated
        // overlay per set.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('rsv10pt5', 'WHT', 'White Flare', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
             VALUES ('rsv10pt5-2', 'rsv10pt5', '2', 2, 'Swadloon')",
            [],
        )
        .unwrap();
        // No masterball_rh printing in the catalog yet (no overlay rule for WHT).
        for v in ["normal", "reverse_holo"] {
            conn.execute(
                "INSERT INTO printings (printing_id, card_id, variant) VALUES (?1, 'rsv10pt5-2', ?2)",
                rusqlite::params![format!("rsv10pt5-2-{v}"), v],
            )
            .unwrap();
        }
        import_groups(
            &mut conn,
            &[TcgGroup {
                group_id: 24326,
                name: "White Flare".into(),
                abbreviation: Some("WHT".into()),
                published_on: None,
            }],
            "2026-05-18",
        )
        .unwrap();

        let products = vec![TcgProduct {
            product_id: 642291,
            group_id: 24326,
            name: "Swadloon (Master Ball Pattern)".into(),
            image_url: None,
            url: None,
            extended_data: vec![ExtendedDatum {
                name: "Number".into(),
                value: "2".into(),
            }],
        }];
        link_card_printings(&mut conn, &products).unwrap();
        // The pattern printing was auto-created and linked.
        let mb_pid: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings WHERE printing_id = 'rsv10pt5-2-masterball_rh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mb_pid, 642291);
        // Base printings still untouched (no base product was passed).
        let normal_pid: Option<i64> = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings WHERE printing_id = 'rsv10pt5-2-normal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(normal_pid, None);
    }

    #[test]
    fn normalize_collector_number_reduces_both_sides() {
        // Modern sets: zero-padded, suffixed with the set total.
        assert_eq!(normalize_collector_number("006/165"), "6");
        assert_eq!(normalize_collector_number("170/165"), "170"); // secret rare
        // Catalog bare form normalizes to the same token.
        assert_eq!(normalize_collector_number("6"), "6");
        // Holo subsets: "H01/H32" must reduce to the catalog's "H1".
        assert_eq!(normalize_collector_number("H01/H32"), "h1");
        assert_eq!(normalize_collector_number("H1"), "h1");
        // Promo namespaces: padded digits inside an alpha prefix.
        assert_eq!(normalize_collector_number("SWSH001"), "swsh1");
        assert_eq!(
            normalize_collector_number("SWSH001"),
            normalize_collector_number("swsh1")
        );
        // Idempotent — re-normalizing changes nothing.
        let once = normalize_collector_number("009/165");
        assert_eq!(normalize_collector_number(&once), once);
    }

    #[test]
    fn links_modern_zero_padded_product_numbers() {
        // The real prod failure mode: pokemontcg.io stores bare "6" but
        // TCGCSV reports the printed "006/165". A raw string compare links
        // nothing; the normalized match must link the card.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('sv3pt5', 'MEW', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
             VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO printings (printing_id, card_id, variant) \
             VALUES ('sv3pt5-6-holo', 'sv3pt5-6', 'holo')",
            [],
        )
        .unwrap();
        import_groups(
            &mut conn,
            &[TcgGroup {
                group_id: 23237,
                name: "151".into(),
                abbreviation: Some("MEW".into()),
                published_on: None,
            }],
            "2026-05-18",
        )
        .unwrap();

        let products = vec![TcgProduct {
            product_id: 5006,
            group_id: 23237,
            name: "Charizard ex - 006/165".into(),
            image_url: None,
            url: None,
            extended_data: vec![ExtendedDatum {
                name: "Number".into(),
                value: "006/165".into(),
            }],
        }];
        let linked = link_card_printings(&mut conn, &products).unwrap();
        assert_eq!(linked, 1, "zero-padded TCGCSV number must still link");
        let pid: i64 = conn
            .query_row(
                "SELECT tcgplayer_product_id FROM printings WHERE printing_id = 'sv3pt5-6-holo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pid, 5006);
    }

    #[test]
    fn normalize_set_name_strips_tcgcsv_era_prefix() {
        // TCGCSV group names carry an era prefix the catalog name lacks.
        assert_eq!(normalize_set_name("SWSH08: Fusion Strike"), "fusion strike");
        assert_eq!(normalize_set_name("Fusion Strike"), "fusion strike");
        assert_eq!(normalize_set_name("SM - Cosmic Eclipse"), "cosmic eclipse");
        assert_eq!(normalize_set_name("Cosmic Eclipse"), "cosmic eclipse");
        // Only the leading era code is a prefix — a later "Crown Zenith:"
        // is part of the real name and must be kept. The catalog set is
        // named "Crown Zenith Galarian Gallery", so both reduce alike.
        assert_eq!(
            normalize_set_name("SWSH: Crown Zenith: Galarian Gallery"),
            "crown zenith galarian gallery"
        );
        assert_eq!(
            normalize_set_name("Crown Zenith Galarian Gallery"),
            "crown zenith galarian gallery"
        );
        // `&` unifies with `and`.
        assert_eq!(normalize_set_name("Sword & Shield"), "sword and shield");
        // A real name that merely contains a hyphen is not mistaken for a
        // prefix — the head before the separator is too long / non-code.
        assert_eq!(
            normalize_set_name("Black and White - Boundaries Crossed"),
            "black and white boundaries crossed"
        );
    }

    #[test]
    fn import_groups_bridges_by_name_when_ptcgo_code_misses() {
        // A set with a ptcgo_code that no group abbreviation matches must
        // still bridge, via the normalized-name fallback.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('swsh8', 'FST', 'Fusion Strike', 'Sword & Shield', '2021/11/12')",
            [],
        )
        .unwrap();
        // The group's abbreviation does NOT equal the set's ptcgo_code.
        let groups = vec![TcgGroup {
            group_id: 2906,
            name: "SWSH08: Fusion Strike".into(),
            abbreviation: Some("FUST".into()),
            published_on: None,
        }];
        import_groups(&mut conn, &groups, "2026-05-19").unwrap();

        let gid: Option<i64> = conn
            .query_row(
                "SELECT tcgcsv_group_id FROM sets WHERE set_code = 'swsh8'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gid, Some(2906), "name fallback must bridge the set");
        let set_code: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 2906",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            set_code.as_deref(),
            Some("swsh8"),
            "both link sides must agree"
        );

        // Idempotent across re-runs.
        import_groups(&mut conn, &groups, "2026-05-19").unwrap();
        let again: Option<i64> = conn
            .query_row(
                "SELECT tcgcsv_group_id FROM sets WHERE set_code = 'swsh8'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(again, Some(2906));
    }

    #[test]
    fn import_groups_keeps_link_sides_consistent() {
        // Many groups share the "PR" promo abbreviation. The group that
        // claims a set must be the one whose `set_code` points back to it,
        // so `link_card_printings` (which trusts `sets.tcgcsv_group_id`)
        // and `import_sealed_products` (which reads `tcgplayer_groups
        // .set_code`) never disagree.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('basep','PR','Wizards Black Star Promos','Promo','1999/07/01'), \
                    ('dpp','PR','DP Black Star Promos','Promo','2007/05/01')",
            [],
        )
        .unwrap();
        let groups = vec![
            TcgGroup {
                group_id: 1421,
                name: "Diamond and Pearl Promos".into(),
                abbreviation: Some("PR".into()),
                published_on: None,
            },
            TcgGroup {
                group_id: 1418,
                name: "WoTC Promo".into(),
                abbreviation: Some("PR".into()),
                published_on: None,
            },
        ];
        import_groups(&mut conn, &groups, "2026-05-19").unwrap();

        // Every set with a group id has a group that points back to it.
        let mismatched: i64 = conn
            .query_row(
                "SELECT count(*) FROM sets s \
                 JOIN tcgplayer_groups g ON s.tcgcsv_group_id = g.group_id \
                 WHERE g.set_code IS NOT s.set_code",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mismatched, 0, "the two link sides must never disagree");

        // Two distinct sets, two distinct group ids — UNIQUE preserved.
        let distinct: i64 = conn
            .query_row(
                "SELECT count(DISTINCT tcgcsv_group_id) FROM sets \
                 WHERE tcgcsv_group_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(distinct, 2);
    }
}
