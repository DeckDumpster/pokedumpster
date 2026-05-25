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
    /// TCGplayer's count of uploaded images for the product. Zero means
    /// the mechanically-generated `image_url` resolves to a 403 — common
    /// for newly-listed cards, Pokemon Center Exclusives, and [Staff]
    /// variants. We treat the URL as unusable when this is 0.
    #[serde(default)]
    pub image_count: i64,
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

/// Hand-curated overlay mapping TCGCSV groups to sets the auto-linker
/// can't reach (abbreviation + normalized-name don't match) or that
/// pokemontcg.io has not yet published. See
/// `data/overrides/tcgcsv_set_bridges.json` for the entries themselves
/// and the per-bridge `comment` field for the rationale.
#[derive(Debug, Clone, Deserialize)]
struct SetBridge {
    tcgcsv_group_id: i64,
    set_code: String,
    /// When present, the bridge first INSERT-OR-IGNOREs a `sets` row
    /// built from these fields before linking — used for groups whose
    /// upstream set entry doesn't exist yet.
    #[serde(default)]
    synthesize: Option<SetSynthesis>,
    /// When true, `synthesize_cards_for_bridges` builds card rows from
    /// the bridged group's TCGCSV products (one row per unique collector
    /// number). Used for sets whose pokemontcg.io entry doesn't yet
    /// exist so the binder isn't a populated tile pointing at zero
    /// cards. INSERT OR IGNORE — never clobbers upstream rows that
    /// arrive later.
    #[serde(default)]
    synthesize_cards: bool,
    /// Free-form note describing why the bridge exists. Not consumed
    /// by code; the bridge file is the documentation surface.
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SetSynthesis {
    ptcgo_code: Option<String>,
    name: String,
    series: String,
    release_date: Option<String>,
    /// Logo/symbol URLs to plant on the synthesized `sets` row. Most
    /// callers point at the parent-series set on pokemontcg.io so the
    /// browse tile + binder symbol match the surrounding aesthetic
    /// rather than rendering a textual ptcgo_code fallback.
    #[serde(default)]
    logo_url: Option<String>,
    #[serde(default)]
    symbol_url: Option<String>,
}

const SET_BRIDGES_JSON: &str = include_str!("../../../data/overrides/tcgcsv_set_bridges.json");

fn load_set_bridges() -> Result<Vec<SetBridge>> {
    Ok(serde_json::from_str(SET_BRIDGES_JSON)?)
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
///
/// Before the auto-link runs, the bridge overlay
/// (`data/overrides/tcgcsv_set_bridges.json`) is applied: it synthesizes
/// any `sets` rows the overlay declares (idempotent INSERT-OR-IGNORE) so
/// the auto-linker can see them, then injects (group_id → set_code)
/// entries that take precedence over abbreviation/name matching.
pub fn import_groups(conn: &mut Connection, groups: &[TcgGroup], now: &str) -> Result<usize> {
    let bridges = load_set_bridges()?;

    // Synthesize sets the overlay declares but the catalog lacks. Done
    // before the auto-link so `resolve_group_set_links` sees them and
    // can't accidentally claim them for the wrong group via tier 1/2.
    {
        let tx = conn.transaction()?;
        for b in &bridges {
            if let Some(synth) = &b.synthesize {
                tx.execute(
                    "INSERT OR IGNORE INTO sets \
                       (set_code, ptcgo_code, name, series, release_date, \
                        logo_url, symbol_url) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![
                        b.set_code,
                        synth.ptcgo_code,
                        synth.name,
                        synth.series,
                        synth.release_date,
                        synth.logo_url,
                        synth.symbol_url,
                    ],
                )?;
                // Heal a synth-owned set row whose synthesize block has
                // grown new fields since it was first inserted (e.g. we
                // later added logo_url/symbol_url to the MEP bridge).
                // `ptcgio_fetched_at IS NULL` scopes this to synth rows
                // — pokemon_tcg_data::upsert_set always stamps that
                // column on upstream-managed rows.
                tx.execute(
                    "UPDATE sets \
                        SET ptcgo_code   = ?2, \
                            name         = ?3, \
                            series       = ?4, \
                            release_date = ?5, \
                            logo_url     = ?6, \
                            symbol_url   = ?7 \
                      WHERE set_code = ?1 \
                        AND ptcgio_fetched_at IS NULL",
                    rusqlite::params![
                        b.set_code,
                        synth.ptcgo_code,
                        synth.name,
                        synth.series,
                        synth.release_date,
                        synth.logo_url,
                        synth.symbol_url,
                    ],
                )?;
            }
        }
        tx.commit()?;
    }

    let mut links = resolve_group_set_links(conn, groups)?;
    // Bridges win: clear any auto-link that would conflict with a
    // bridged set_code, then apply the bridge mapping directly.
    for b in &bridges {
        links.retain(|_, v| v != &b.set_code);
        links.insert(b.tcgcsv_group_id, b.set_code.clone());
    }

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

/// For each bridge entry with `synthesize_cards: true`, build `cards`
/// rows from TCGCSV products in the bridged group (one row per unique
/// collector number). Sourcing the canonical name + image: prefer
/// products with no parenthetical tag and no `[Staff]` marker — that's
/// the bare base product. Falls back to any product matching the
/// number when no bare-name one exists.
///
/// Two writes per card:
///   - `INSERT OR IGNORE` to create the row when missing, so synth never
///     overwrites a real upstream pokemontcg.io row.
///   - A follow-on `UPDATE` to refresh name + images for synth-owned rows
///     specifically — rows where `image_large` is NULL or points at
///     tcgplayer-cdn (synth's only ever source). This is what heals an
///     earlier refresh that shipped a known-broken URL once TCGCSV's
///     imageCount transitions to non-zero or a sibling product gains an
///     image.
///
/// The canonical product per collector number sources `name`. The image
/// prefers the canonical's `image_url`, falling back to any sibling
/// product (same number, different treatment) when the canonical has
/// none — Pokemon Center Exclusives sometimes have an image while the
/// bare-name base does not.
///
/// Returns the count of `cards` rows freshly inserted by this call (does
/// not count UPDATEs to existing synth-owned rows).
pub fn synthesize_cards_for_bridges(conn: &mut Connection) -> Result<usize> {
    let bridges = load_set_bridges()?;
    let tx = conn.transaction()?;
    let mut n = 0;
    for b in &bridges {
        if !b.synthesize_cards {
            continue;
        }
        // Pull every product in the bridged group with a collector
        // number. Order so bare-name products (no `[`, no `(`) come
        // first — the first row per number becomes the canonical source.
        let mut stmt = tx.prepare(
            "SELECT product_id, name, collector_number, image_url, rarity \
               FROM tcgcsv_products \
              WHERE group_id = ?1 AND collector_number IS NOT NULL \
              ORDER BY \
                (CASE WHEN name LIKE '%[%' OR name LIKE '%(%' THEN 1 ELSE 0 END), \
                product_id",
        )?;
        // (product_id, name, collector_number, image_url, rarity).
        type SynthRow = (i64, String, String, Option<String>, Option<String>);
        // (name, image_url, rarity) for each product sharing a number.
        type ProductFields = (String, Option<String>, Option<String>);
        let rows: Vec<SynthRow> = stmt
            .query_map([b.tcgcsv_group_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        // Group products by normalized collector number, preserving the
        // canonical-first ordering above so the first entry per group is
        // always the source of `name` (with image/rarity falling back to
        // any sibling that has them).
        let mut by_number: std::collections::BTreeMap<String, Vec<ProductFields>> =
            std::collections::BTreeMap::new();
        for (_product_id, name, number, image_url, rarity) in rows {
            let normalized = normalize_collector_number(&number);
            by_number
                .entry(normalized)
                .or_default()
                .push((name, image_url, rarity));
        }

        for (number, products) in by_number {
            let canonical = &products[0];
            let card_name = pkdump_core::variant::parse_product_card_name(&canonical.0);
            let image = products.iter().find_map(|(_, u, _)| u.as_deref());
            let rarity = products.iter().find_map(|(_, _, r)| r.as_deref());
            let card_id = format!("{}-{}", b.set_code, number);
            let sortable = pkdump_core::number_sortable(&number);

            tx.execute(
                "INSERT OR IGNORE INTO cards \
                   (card_id, set_code, number, number_sortable, name, rarity, \
                    image_small, image_large) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                rusqlite::params![
                    card_id, b.set_code, number, sortable, card_name, rarity, image
                ],
            )?;
            n += tx.changes() as usize;

            // Heal a synth-owned row whose image/rarity needs updating
            // (the canonical or fallback may have changed since last
            // refresh, or the previous run wrote a now-known-broken
            // URL). The `raw_json IS NULL` predicate scopes the UPDATE
            // to synth-managed rows — pokemon_tcg_data::upsert_card
            // always writes raw_json on upstream rows, so it's the
            // cleanest signal that a row came from synth rather than
            // upstream.
            tx.execute(
                "UPDATE cards \
                    SET name        = ?2, \
                        rarity      = ?3, \
                        image_small = ?4, \
                        image_large = ?4 \
                  WHERE card_id = ?1 \
                    AND raw_json IS NULL",
                rusqlite::params![card_id, card_name, rarity, image],
            )?;
        }
    }
    tx.commit()?;
    Ok(n)
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

/// Persist single-card TCGplayer products to `tcgcsv_products`. Variant
/// expansion (in `crate::overrides::expand_all_printings`) reads this table
/// to resolve which printings actually exist for each card, so the
/// `derived_variant` column is pre-computed here from both pattern and
/// stamp parsers — stamp products that live in the card's own group
/// (e.g. MEP's "Alakazam - 003 [Staff]") would otherwise fall through
/// to the base-product branch and steal another variant's printing.
pub fn import_products(conn: &mut Connection, products: &[TcgProduct], now: &str) -> Result<usize> {
    let tx = conn.transaction()?;
    let mut n = 0;
    for product in products {
        if !is_single_card(product) {
            continue;
        }
        let number = product
            .extended_data
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("Number"))
            .map(|e| e.value.as_str())
            .unwrap_or_default()
            .trim();
        if number.is_empty() {
            continue;
        }
        let rarity = product
            .extended_data
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case("Rarity"))
            .map(|e| e.value.trim().to_string())
            .filter(|s| !s.is_empty());
        let derived: Option<String> =
            pkdump_core::variant::variant_from_product_name(&product.name)
                .map(|s| s.to_string())
                .or_else(|| pkdump_core::variant::parse_stamp_tag(&product.name).map(|(v, _)| v));
        // TCGCSV's image_url is mechanically generated and 403s when the
        // product has no uploaded image yet (imageCount=0). Don't carry
        // the known-broken URL forward — synth's per-number fallback
        // picks up a sibling product's image when this one has none.
        let image_url = if product.image_count > 0 {
            product.image_url.as_deref()
        } else {
            None
        };
        tx.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, \
                image_url, rarity, fetched_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(product_id) DO UPDATE SET \
               group_id         = excluded.group_id, \
               name             = excluded.name, \
               collector_number = excluded.collector_number, \
               derived_variant  = excluded.derived_variant, \
               image_url        = excluded.image_url, \
               rarity           = excluded.rarity, \
               fetched_at       = excluded.fetched_at",
            rusqlite::params![
                product.product_id,
                product.group_id,
                product.name,
                number,
                derived,
                image_url,
                rarity,
                now
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
            image_count: 0,
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
            image_count: 0,
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
                image_count: 0,
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
                image_count: 0,
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
            image_count: 0,
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

    #[test]
    fn import_groups_applies_explicit_bridge_for_svp() {
        // The svp set ships from pokemontcg.io with ptcgo_code 'PR-SV'
        // and name 'Scarlet & Violet Black Star Promos'. The auto-linker
        // can't bridge it to TCGCSV's group 22872 'SV: Scarlet & Violet
        // Promo Cards' (abbreviation 'SVP') — neither the abbreviations
        // nor the normalized names match. The bridge overlay supplies
        // the link explicitly.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('svp','PR-SV','Scarlet & Violet Black Star Promos','Scarlet & Violet','2023/01/01')",
            [],
        )
        .unwrap();
        let groups = vec![TcgGroup {
            group_id: 22872,
            name: "SV: Scarlet & Violet Promo Cards".into(),
            abbreviation: Some("SVP".into()),
            published_on: Some("2023-03-31T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-05-24").unwrap();

        let gid: Option<i64> = conn
            .query_row(
                "SELECT tcgcsv_group_id FROM sets WHERE set_code = 'svp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gid, Some(22872), "bridge links svp to TCGCSV group 22872");

        let set_code: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 22872",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(set_code.as_deref(), Some("svp"));
    }

    #[test]
    fn import_groups_synthesizes_orphan_promo_set_for_mep() {
        // pokemontcg.io has not published a Mega Evolution Promo set
        // entry (as of 2026-05-24, ~8 months past TCGCSV's release).
        // The bridge overlay synthesizes a sets row from TCGCSV group
        // 24451's metadata so the set appears in /browse.
        let (_d, mut conn) = shared_db();
        let groups = vec![TcgGroup {
            group_id: 24451,
            name: "ME: Mega Evolution Promo".into(),
            abbreviation: Some("MEP".into()),
            published_on: Some("2025-09-26T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-05-24").unwrap();

        let row: (String, Option<String>, String, Option<i64>) = conn
            .query_row(
                "SELECT set_code, ptcgo_code, name, tcgcsv_group_id \
                   FROM sets WHERE set_code = 'mep'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "mep".into(),
                Some("MEP".into()),
                "ME Black Star Promos".into(),
                Some(24451),
            )
        );
    }

    #[test]
    fn import_products_persists_rarity_from_extended_data() {
        // TCGCSV ships "Rarity" alongside "Number" in extendedData
        // — typically "Promo" for items in promo groups. Capturing it
        // lets synth-cards render the same rarity glyph other cards
        // get instead of looking blank.
        let (_d, mut conn) = shared_db();
        let products = vec![TcgProduct {
            product_id: 694694,
            group_id: 24451,
            name: "Fennekin - 080".into(),
            image_url: Some("https://tcgplayer-cdn.tcgplayer.com/product/694694_200w.jpg".into()),
            url: None,
            image_count: 1,
            extended_data: vec![
                ExtendedDatum {
                    name: "Number".into(),
                    value: "080".into(),
                },
                ExtendedDatum {
                    name: "Rarity".into(),
                    value: "Promo".into(),
                },
            ],
        }];
        import_products(&mut conn, &products, "2026-05-25").unwrap();
        let rarity: Option<String> = conn
            .query_row(
                "SELECT rarity FROM tcgcsv_products WHERE product_id = 694694",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rarity.as_deref(), Some("Promo"));
    }

    #[test]
    fn synthesize_cards_writes_rarity_from_canonical_product() {
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, tcgcsv_group_id) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution', 24451)",
            [],
        )
        .unwrap();
        let products = vec![TcgProduct {
            product_id: 654594,
            group_id: 24451,
            name: "Meganium - 001".into(),
            image_url: Some("https://tcgplayer.example/654594.jpg".into()),
            url: None,
            image_count: 1,
            extended_data: vec![
                ExtendedDatum {
                    name: "Number".into(),
                    value: "001".into(),
                },
                ExtendedDatum {
                    name: "Rarity".into(),
                    value: "Promo".into(),
                },
            ],
        }];
        import_products(&mut conn, &products, "2026-05-25").unwrap();
        synthesize_cards_for_bridges(&mut conn).unwrap();
        let rarity: Option<String> = conn
            .query_row(
                "SELECT rarity FROM cards WHERE card_id = 'mep-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rarity.as_deref(), Some("Promo"));
    }

    #[test]
    fn synthesize_set_carries_logo_and_symbol_urls() {
        // The bridge's synthesize block may supply logo_url + symbol_url
        // (e.g. point MEP at the Mega Evolution parent set's art so the
        // synthesized set tile matches the other Black Star Promos
        // aesthetically). Tested via the existing MEP bridge entry.
        let (_d, mut conn) = shared_db();
        let groups = vec![TcgGroup {
            group_id: 24451,
            name: "ME: Mega Evolution Promo".into(),
            abbreviation: Some("MEP".into()),
            published_on: Some("2025-09-26T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-05-25").unwrap();
        let row: (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT logo_url, symbol_url FROM sets WHERE set_code = 'mep'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            row.0.as_deref(),
            Some("https://images.pokemontcg.io/me1/logo.png"),
            "MEP inherits the Mega Evolution parent-set logo"
        );
        assert_eq!(
            row.1.as_deref(),
            Some("https://images.pokemontcg.io/me1/symbol.png")
        );
    }

    #[test]
    fn synthesize_set_self_heals_when_bridge_gains_fields() {
        // Prod state on 2026-05-25: mep was synthesized before the bridge
        // had logo_url/symbol_url, so the existing row carries NULL there.
        // Subsequent refreshes must self-heal — INSERT OR IGNORE alone
        // would leave the row half-populated. The heal scopes to
        // `ptcgio_fetched_at IS NULL` so upstream-managed sets stay
        // untouched.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution', '2025/09/26')",
            [],
        )
        .unwrap();
        let groups = vec![TcgGroup {
            group_id: 24451,
            name: "ME: Mega Evolution Promo".into(),
            abbreviation: Some("MEP".into()),
            published_on: Some("2025-09-26T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-05-25").unwrap();
        let logo: Option<String> = conn
            .query_row(
                "SELECT logo_url FROM sets WHERE set_code = 'mep'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            logo.as_deref(),
            Some("https://images.pokemontcg.io/me1/logo.png"),
            "pre-existing synth row picks up new bridge fields"
        );
    }

    #[test]
    fn synthesize_set_does_not_clobber_upstream_pokemontcg_row() {
        // If pokemontcg.io eventually publishes a competing set under
        // the same set_code, the upstream row (marked by a non-NULL
        // ptcgio_fetched_at) wins — synth's self-heal must leave it
        // alone.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date, \
                               logo_url, symbol_url, ptcgio_fetched_at) \
             VALUES ('mep', 'REAL', 'Real Upstream MEP', 'Real Series', \
                     '2026/01/01', \
                     'https://images.pokemontcg.io/mep/logo.png', \
                     'https://images.pokemontcg.io/mep/symbol.png', \
                     '2026-05-25T00:00:00')",
            [],
        )
        .unwrap();
        let groups = vec![TcgGroup {
            group_id: 24451,
            name: "ME: Mega Evolution Promo".into(),
            abbreviation: Some("MEP".into()),
            published_on: Some("2025-09-26T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-05-25").unwrap();
        let name: String = conn
            .query_row("SELECT name FROM sets WHERE set_code = 'mep'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Real Upstream MEP", "upstream row untouched");
    }

    #[test]
    fn import_products_drops_image_url_when_image_count_is_zero() {
        // TCGCSV's image_url is mechanically generated; it 403s for
        // products whose imageCount is 0 (newly listed, Pokemon Center
        // Exclusives, [Staff] variants). Storing the URL anyway would
        // ship a known-broken image to the binder.
        let (_d, mut conn) = shared_db();
        let products = vec![TcgProduct {
            product_id: 694694,
            group_id: 24451,
            name: "Fennekin - 080".into(),
            image_url: Some("https://tcgplayer-cdn.tcgplayer.com/product/694694_200w.jpg".into()),
            url: None,
            image_count: 0,
            extended_data: vec![ExtendedDatum {
                name: "Number".into(),
                value: "080".into(),
            }],
        }];
        import_products(&mut conn, &products, "2026-05-24").unwrap();
        let url: Option<String> = conn
            .query_row(
                "SELECT image_url FROM tcgcsv_products WHERE product_id = 694694",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            url, None,
            "image_url must be NULL when TCGCSV reports imageCount=0"
        );
    }

    #[test]
    fn import_products_persists_image_url() {
        let (_d, mut conn) = shared_db();
        let products = vec![TcgProduct {
            product_id: 654594,
            group_id: 24451,
            name: "Meganium - 001".into(),
            image_url: Some("https://tcgplayer.example/654594.jpg".into()),
            url: None,
            image_count: 1,
            extended_data: vec![ExtendedDatum {
                name: "Number".into(),
                value: "001".into(),
            }],
        }];
        import_products(&mut conn, &products, "2026-05-24").unwrap();
        let url: Option<String> = conn
            .query_row(
                "SELECT image_url FROM tcgcsv_products WHERE product_id = 654594",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(url.as_deref(), Some("https://tcgplayer.example/654594.jpg"));
    }

    #[test]
    fn import_products_derives_stamp_variant_for_staff_products() {
        // Stamp products that live in the card's own group (not MCAP)
        // must pre-resolve to a stamp_* variant here, so variant
        // expansion's own-group path picks them up via derived_variant
        // instead of mis-classifying them as base products.
        let (_d, mut conn) = shared_db();
        let products = vec![
            TcgProduct {
                product_id: 656385,
                group_id: 24451,
                name: "Alakazam - 003 [Staff]".into(),
                image_url: None,
                url: None,
                image_count: 0,
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "003".into(),
                }],
            },
            TcgProduct {
                product_id: 663187,
                group_id: 24451,
                name: "Ceruledge (Prerelease)".into(),
                image_url: None,
                url: None,
                image_count: 0,
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "014".into(),
                }],
            },
        ];
        import_products(&mut conn, &products, "2026-05-24").unwrap();
        let staff: Option<String> = conn
            .query_row(
                "SELECT derived_variant FROM tcgcsv_products WHERE product_id = 656385",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(staff.as_deref(), Some("stamp_staff"));
        let pre: Option<String> = conn
            .query_row(
                "SELECT derived_variant FROM tcgcsv_products WHERE product_id = 663187",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pre.as_deref(), Some("stamp_prerelease"));
    }

    #[test]
    fn synthesize_cards_for_bridges_populates_mep_from_tcgcsv_products() {
        // MEP has no pokemontcg.io counterpart, so the bridge declares
        // synthesize_cards: true and the synthesis step builds card rows
        // from TCGCSV's product list. One row per unique collector
        // number, sourced from the canonical bare-name product when
        // available (the [Staff] / Pokemon Center Exclusive variants
        // share a number with the base card).
        let (_d, mut conn) = shared_db();
        // The MEP set row is pre-seeded by import_groups (via the
        // bridge); for this unit test we seed it directly.
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, tcgcsv_group_id) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution', 24451)",
            [],
        )
        .unwrap();
        let products = vec![
            // Base card: source of the canonical name + image.
            TcgProduct {
                product_id: 654597,
                group_id: 24451,
                name: "Alakazam - 003".into(),
                image_url: Some("https://tcgplayer.example/654597.jpg".into()),
                url: None,
                image_count: 1,
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "003".into(),
                }],
            },
            // [Staff] of the same card — must NOT create a second row.
            TcgProduct {
                product_id: 656385,
                group_id: 24451,
                name: "Alakazam - 003 [Staff]".into(),
                image_url: None,
                url: None,
                image_count: 0,
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "003".into(),
                }],
            },
            // Different card, only a (Prerelease) product exists — the
            // base name still parses out via parse_product_card_name.
            TcgProduct {
                product_id: 663187,
                group_id: 24451,
                name: "Ceruledge (Prerelease)".into(),
                image_url: Some("https://tcgplayer.example/663187.jpg".into()),
                url: None,
                image_count: 1,
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "014".into(),
                }],
            },
        ];
        import_products(&mut conn, &products, "2026-05-24").unwrap();
        let synth = synthesize_cards_for_bridges(&mut conn).unwrap();
        assert_eq!(synth, 2, "two unique collector numbers → two cards");

        let (alakazam_name, alakazam_img): (String, Option<String>) = conn
            .query_row(
                "SELECT name, image_large FROM cards WHERE card_id = 'mep-3'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(alakazam_name, "Alakazam");
        assert_eq!(
            alakazam_img.as_deref(),
            Some("https://tcgplayer.example/654597.jpg"),
            "base product is the canonical source for image"
        );

        let ceruledge_name: String = conn
            .query_row("SELECT name FROM cards WHERE card_id = 'mep-14'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(ceruledge_name, "Ceruledge");
    }

    #[test]
    fn synthesize_cards_falls_back_to_sibling_image_when_canonical_has_none() {
        // Fennekin - 080 is the bare-name canonical (TCGCSV has no
        // uploaded image yet → image_url NULL), but Fennekin - 080
        // (Pokemon Center Exclusive) sharing the same number does have
        // one. The synth should reach across to that sibling rather
        // than ship a card with no image.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, tcgcsv_group_id) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution', 24451)",
            [],
        )
        .unwrap();
        let products = vec![
            TcgProduct {
                product_id: 694694,
                group_id: 24451,
                name: "Fennekin - 080".into(),
                image_url: Some(
                    "https://tcgplayer-cdn.tcgplayer.com/product/694694_200w.jpg".into(),
                ),
                url: None,
                image_count: 0, // canonical, but no image
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "080".into(),
                }],
            },
            TcgProduct {
                product_id: 694695,
                group_id: 24451,
                name: "Fennekin - 080 (Pokemon Center Exclusive)".into(),
                image_url: Some(
                    "https://tcgplayer-cdn.tcgplayer.com/product/694695_200w.jpg".into(),
                ),
                url: None,
                image_count: 1, // sibling has the image
                extended_data: vec![ExtendedDatum {
                    name: "Number".into(),
                    value: "080".into(),
                }],
            },
        ];
        import_products(&mut conn, &products, "2026-05-24").unwrap();
        synthesize_cards_for_bridges(&mut conn).unwrap();

        let (name, img): (String, Option<String>) = conn
            .query_row(
                "SELECT name, image_large FROM cards WHERE card_id = 'mep-80'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Fennekin", "name still sourced from canonical");
        assert_eq!(
            img.as_deref(),
            Some("https://tcgplayer-cdn.tcgplayer.com/product/694695_200w.jpg"),
            "image falls back to sibling product when canonical has none"
        );
    }

    #[test]
    fn synthesize_cards_refreshes_existing_synth_owned_row() {
        // Prod state on 2026-05-25: mep-80 was written with the broken
        // canonical URL during yesterday's refresh. After the
        // imageCount fix, the next refresh must heal the row by
        // updating its image_large — INSERT OR IGNORE alone would leave
        // the broken URL in place. Synth-owned rows are recognised by
        // their image_large pointing at tcgplayer-cdn (upstream uses
        // images.pokemontcg.io).
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, tcgcsv_group_id) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution', 24451)",
            [],
        )
        .unwrap();
        // Pre-existing synth-owned row with the broken URL we shipped
        // before honoring imageCount.
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, image_small, image_large) \
             VALUES ('mep-80', 'mep', '80', 80, 'Fennekin', \
                     'https://tcgplayer-cdn.tcgplayer.com/product/694694_200w.jpg', \
                     'https://tcgplayer-cdn.tcgplayer.com/product/694694_200w.jpg')",
            [],
        )
        .unwrap();
        // After the imageCount fix, 694694 has no image_url in our
        // table; the PC Exclusive sibling does.
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, image_url, fetched_at) \
             VALUES (694694, 24451, 'Fennekin - 080', '080', NULL, NULL, '2026-05-25'), \
                    (694695, 24451, 'Fennekin - 080 (Pokemon Center Exclusive)', '080', NULL, \
                     'https://tcgplayer-cdn.tcgplayer.com/product/694695_200w.jpg', '2026-05-25')",
            [],
        )
        .unwrap();
        synthesize_cards_for_bridges(&mut conn).unwrap();
        let img: Option<String> = conn
            .query_row(
                "SELECT image_large FROM cards WHERE card_id = 'mep-80'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            img.as_deref(),
            Some("https://tcgplayer-cdn.tcgplayer.com/product/694695_200w.jpg"),
            "synth-owned row was refreshed from sibling image after imageCount fix"
        );
    }

    #[test]
    fn synthesize_cards_does_not_clobber_upstream_pokemontcg_row() {
        // The refresh contract: upstream pokemontcg.io upserts are
        // authoritative for any card they cover. Synth must never
        // overwrite a row whose image_large points at
        // images.pokemontcg.io, even if the bridge happens to have the
        // same set_code as an upstream-managed set.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, tcgcsv_group_id) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution', 24451)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, image_large, raw_json) \
             VALUES ('mep-99', 'mep', '99', 99, 'Real Upstream Name', \
                     'https://images.pokemontcg.io/mep/99_hires.png', '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tcgcsv_products \
               (product_id, group_id, name, collector_number, derived_variant, image_url, fetched_at) \
             VALUES (999999, 24451, 'Other Name - 099', '099', NULL, \
                     'https://tcgplayer-cdn.tcgplayer.com/product/999999_200w.jpg', '2026-05-25')",
            [],
        )
        .unwrap();
        synthesize_cards_for_bridges(&mut conn).unwrap();
        let (name, img): (String, String) = conn
            .query_row(
                "SELECT name, image_large FROM cards WHERE card_id = 'mep-99'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Real Upstream Name");
        assert_eq!(img, "https://images.pokemontcg.io/mep/99_hires.png");
    }

    #[test]
    fn synthesize_cards_is_idempotent_and_skips_existing() {
        // Running synthesize twice must not duplicate; a pre-existing
        // card row (real catalog data) must be preserved untouched.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, tcgcsv_group_id) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution', 24451)",
            [],
        )
        .unwrap();
        // Pre-existing card row from upstream (hypothetical). What
        // marks it as upstream-managed is the populated `raw_json` —
        // pokemon_tcg_data::upsert_card always writes it, synth never
        // does. The synth step's UPDATE must skip it.
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, raw_json) \
             VALUES ('mep-3', 'mep', '3', 3, 'Real Upstream Name', '{}')",
            [],
        )
        .unwrap();
        let products = vec![TcgProduct {
            product_id: 654597,
            group_id: 24451,
            name: "Alakazam - 003".into(),
            image_url: Some("https://tcgplayer.example/654597.jpg".into()),
            url: None,
            image_count: 1,
            extended_data: vec![ExtendedDatum {
                name: "Number".into(),
                value: "003".into(),
            }],
        }];
        import_products(&mut conn, &products, "2026-05-24").unwrap();
        synthesize_cards_for_bridges(&mut conn).unwrap();
        synthesize_cards_for_bridges(&mut conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM cards WHERE card_id = 'mep-3'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let name: String = conn
            .query_row("SELECT name FROM cards WHERE card_id = 'mep-3'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Real Upstream Name", "existing row not clobbered");
    }

    #[test]
    fn import_groups_bridges_are_idempotent_across_reruns() {
        // Running import_groups twice with the same groups list must
        // converge — synthesized sets aren't duplicated, links stay put.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('svp','PR-SV','Scarlet & Violet Black Star Promos','Scarlet & Violet','2023/01/01')",
            [],
        )
        .unwrap();
        let groups = vec![
            TcgGroup {
                group_id: 22872,
                name: "SV: Scarlet & Violet Promo Cards".into(),
                abbreviation: Some("SVP".into()),
                published_on: Some("2023-03-31T00:00:00".into()),
            },
            TcgGroup {
                group_id: 24451,
                name: "ME: Mega Evolution Promo".into(),
                abbreviation: Some("MEP".into()),
                published_on: Some("2025-09-26T00:00:00".into()),
            },
        ];
        import_groups(&mut conn, &groups, "2026-05-24").unwrap();
        import_groups(&mut conn, &groups, "2026-05-25").unwrap();

        // svp still linked to 22872.
        let svp_gid: Option<i64> = conn
            .query_row(
                "SELECT tcgcsv_group_id FROM sets WHERE set_code = 'svp'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(svp_gid, Some(22872));
        // mep was synthesized exactly once, still linked to 24451.
        let mep_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sets WHERE set_code = 'mep'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mep_count, 1, "synthesized set must not duplicate on re-run");
        let mep_gid: Option<i64> = conn
            .query_row(
                "SELECT tcgcsv_group_id FROM sets WHERE set_code = 'mep'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mep_gid, Some(24451));
    }
}
