//! Pokémon Japan catalog ingest — TCGCSV categoryId 85.
//!
//! Japan has no pokemontcg.io counterpart, so unlike the English catalog
//! (repo bulk import → API tail → TCGCSV bridge) every Japanese set and
//! card is synthesized from TCGCSV alone. This module is the whole
//! pipeline for that category:
//!
//!   1. [`import_groups`] — one `sets` row + one `tcgplayer_groups` row
//!      per TCGCSV group, `set_code` = `jp-<group_id>`.
//!   2. [`import_products`] — split each group's products into cards
//!      (`tcgcsv_products`) and sealed (`sealed_products`).
//!   3. [`synthesize_cards`] — build `cards` rows from the products'
//!      `extendedData`.
//!
//! Prices, variant expansion, and the materialized latest-price table are
//! *not* duplicated here: JP products land in the same `tcgcsv_products` /
//! `prices` tables as English ones, so `tcgcsv::import_prices` and
//! `overrides::expand_all_printings` pick them up unchanged.
//!
//! Two things make Japan different from every English group, and both are
//! handled here rather than by widening the English path:
//!
//! **No auto-linking.** `tcgcsv::import_groups` bridges a group to a
//! catalog set by abbreviation and by normalized name. Running Japanese
//! groups through it would let "Pokemon Jungle" claim the English `base2`
//! set and "SV2a: Pokemon Card 151" claim `sv3pt5`. JP groups get their
//! `jp-<group_id>` set built for them and never consult the English
//! catalog.
//!
//! **No collector numbers before ~2010.** TCGCSV's `Number` extendedData
//! is absent on ~2.4k vintage Japanese products (Mystery of the Fossils,
//! City Gym Decks, Leaders' Stadium, …). `Number` is what the English path
//! uses to tell a card from a sealed product, so JP discriminates on
//! `CardType` instead, and numberless cards take the synthetic number
//! `p<product_id>` — stable across refreshes, unique, and visibly not a
//! printed collector number. Inventing a 1..N sequence would look real and
//! be wrong: TCGCSV lists these groups alphabetically, not in set order.

use std::collections::{BTreeMap, HashMap};

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::Result;
use crate::tcgcsv::{
    CATEGORY_POKEMON_JAPAN, ExtendedDatum, TcgGroup, TcgProduct, TcgcsvClient, classify_sealed,
    normalize_collector_number,
};

/// Prefix every synthesized Japanese `set_code` carries. The suffix is the
/// TCGCSV group id — abbreviations are empty on 40% of JP groups and
/// duplicated across the rest ("DP1", "sA", "Pt" each name several), so
/// the group id is the only stable unique key.
pub const SET_CODE_PREFIX: &str = "jp-";

/// The catalog `set_code` for a TCGCSV Japanese group.
pub fn set_code(group_id: i64) -> String {
    format!("{SET_CODE_PREFIX}{group_id}")
}

// ---------------------------------------------------------------------
// Series buckets (data/japan_series.json)
// ---------------------------------------------------------------------

const JAPAN_SERIES_JSON: &str = include_str!("../../../data/japan_series.json");

#[derive(Debug, Deserialize)]
struct SeriesSeed {
    undated_series: String,
    eras: Vec<EraSeed>,
    #[serde(default)]
    name_overrides: Vec<NameOverrideSeed>,
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct EraSeed {
    from: String,
    series: String,
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NameOverrideSeed {
    name: String,
    series: String,
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<String>,
}

/// Resolves a Japanese group to its `sets.series` bucket. Authored in
/// `data/japan_series.json`; see that file for the era boundaries.
#[derive(Debug, Clone)]
pub struct SeriesMap {
    undated: String,
    /// (YYYYMMDD, series), ascending by date.
    eras: Vec<(String, String)>,
    overrides: HashMap<String, String>,
}

impl SeriesMap {
    /// Parse the embedded seed.
    pub fn load() -> Result<Self> {
        let seed: SeriesSeed = serde_json::from_str(JAPAN_SERIES_JSON)?;
        let mut eras: Vec<(String, String)> = seed
            .eras
            .into_iter()
            .map(|e| (digits(&e.from), e.series))
            .collect();
        eras.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self {
            undated: seed.undated_series,
            eras,
            overrides: seed
                .name_overrides
                .into_iter()
                .map(|o| (o.name, o.series))
                .collect(),
        })
    }

    /// The series bucket for a group. A `name_overrides` entry wins; then
    /// the last era whose `from` date is on or before `release_date`;
    /// undated groups (and any group predating the first era) fall to
    /// `undated_series`.
    pub fn series_for(&self, name: &str, release_date: Option<&str>) -> &str {
        if let Some(series) = self.overrides.get(name) {
            return series;
        }
        let Some(date) = release_date.map(digits) else {
            return &self.undated;
        };
        self.eras
            .iter()
            .rev()
            .find(|(from, _)| from.as_str() <= date.as_str())
            .map(|(_, series)| series.as_str())
            .unwrap_or(&self.undated)
    }
}

/// Reduce a date to its digits so `"1996-01-01"` and `"1996/01/01"`
/// compare lexicographically as the same `"19960101"`.
fn digits(date: &str) -> String {
    date.chars().filter(char::is_ascii_digit).collect()
}

/// A group's release date in the catalog's `YYYY/MM/DD` convention, or
/// `None` when TCGCSV has no real date for it.
///
/// TCGCSV fills the `publishedOn` of undated groups with the timestamp of
/// the response itself (`"2026-07-30T20:16:11.0447291Z"`), which would
/// otherwise file 17 vintage promo groups as brand-new releases. Real
/// dates are always exactly midnight with no fractional part or zone, so
/// that's the shape this accepts.
pub fn release_date(published_on: Option<&str>) -> Option<String> {
    let raw = published_on?;
    let (date, time) = raw.split_once('T')?;
    if time != "00:00:00" || date.len() != 10 {
        return None;
    }
    Some(date.replace('-', "/"))
}

// ---------------------------------------------------------------------
// Groups → sets
// ---------------------------------------------------------------------

/// Import Japanese groups: one synthesized `sets` row and one
/// `tcgplayer_groups` row apiece. Returns the number of groups.
///
/// The `sets` upsert deliberately leaves `total` / `printed_total` alone —
/// [`synthesize_cards`] derives `printed_total` from the group's collector
/// numbers once the products are in, and re-running group import must not
/// wipe it.
pub fn import_groups(conn: &mut Connection, groups: &[TcgGroup], now: &str) -> Result<usize> {
    let series = SeriesMap::load()?;
    let tx = conn.transaction()?;
    for g in groups {
        let code = set_code(g.group_id);
        let released = release_date(g.published_on.as_deref());
        let bucket = series.series_for(&g.name, released.as_deref());
        let abbreviation = g.abbreviation.as_deref().filter(|a| !a.is_empty());
        tx.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(set_code) DO UPDATE SET \
               ptcgo_code   = excluded.ptcgo_code, \
               name         = excluded.name, \
               series       = excluded.series, \
               release_date = excluded.release_date",
            rusqlite::params![code, abbreviation, g.name, bucket, released],
        )?;
        tx.execute(
            "INSERT INTO tcgplayer_groups \
               (group_id, set_code, name, abbreviation, published_on, fetched_at, role) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'primary') \
             ON CONFLICT(group_id) DO UPDATE SET \
               set_code     = excluded.set_code, \
               name         = excluded.name, \
               abbreviation = excluded.abbreviation, \
               published_on = excluded.published_on, \
               fetched_at   = excluded.fetched_at, \
               role         = excluded.role",
            rusqlite::params![g.group_id, code, g.name, abbreviation, g.published_on, now,],
        )?;
    }
    tx.commit()?;
    Ok(groups.len())
}

// ---------------------------------------------------------------------
// Products
// ---------------------------------------------------------------------

/// Whether a Japanese product is a single card.
///
/// The English path keys off the `Number` extendedData entry, which every
/// Japanese product from ~2010 onward also carries — but ~2.4k vintage
/// ones don't. `CardType` is present on every Japanese card and on no
/// sealed product, so that's the discriminator here.
pub fn is_card(product: &TcgProduct) -> bool {
    extended(product, "CardType").is_some()
}

/// The collector number a Japanese product is filed under. Falls back to
/// the synthetic `p<product_id>` form for the vintage groups TCGCSV
/// publishes without numbers — see the module docs.
pub fn collector_number(product: &TcgProduct) -> String {
    match extended(product, "Number") {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        _ => format!("p{}", product.product_id),
    }
}

/// Persist a group's products: cards into `tcgcsv_products` (which variant
/// expansion reads back), everything else into `sealed_products`. Returns
/// (card products, sealed products).
pub fn import_products(
    conn: &mut Connection,
    products: &[TcgProduct],
    now: &str,
) -> Result<(usize, usize)> {
    let tx = conn.transaction()?;
    let mut cards = 0;
    let mut sealed = 0;
    for p in products {
        if !is_card(p) {
            tx.execute(
                "INSERT INTO sealed_products \
                   (product_id, set_code, name, category, image_url, tcgplayer_url, fetched_at) \
                 VALUES (?1, \
                         (SELECT set_code FROM tcgplayer_groups WHERE group_id = ?2), \
                         ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT(product_id) DO UPDATE SET \
                   set_code      = excluded.set_code, \
                   name          = excluded.name, \
                   category      = excluded.category, \
                   image_url     = excluded.image_url, \
                   tcgplayer_url = excluded.tcgplayer_url, \
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
            sealed += 1;
            continue;
        }
        // Only the treatment-pattern parser runs on Japanese names.
        // `(Mirror Holofoil)` is the one printing treatment TCGCSV tags
        // in the name; the rest of the parentheticals are card identity
        // ("(Delta Species)", "(Team Plasma)") or artist credits, which
        // `treatment_for` already declines. `parse_stamp_tag` is skipped
        // outright — Japan has no cross-group stamped-promo catch-all
        // for it to bridge.
        let derived = pkdump_core::variant::variant_from_product_name(&p.name);
        let image_url = if p.image_count > 0 {
            p.image_url.as_deref()
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
                p.product_id,
                p.group_id,
                p.name,
                collector_number(p),
                derived,
                image_url,
                rarity(p),
                now,
            ],
        )?;
        cards += 1;
    }
    tx.commit()?;
    Ok((cards, sealed))
}

// ---------------------------------------------------------------------
// Cards
// ---------------------------------------------------------------------

/// Build `cards` rows for one Japanese group from its products'
/// `extendedData`, and derive the set's `printed_total` from their
/// collector numbers. Returns the number of distinct cards written.
///
/// One card per distinct normalized collector number: TCGCSV lists the
/// alternate-art and mirror-holo printings of a card as separate products
/// sharing its number, and those are printings, not cards. The canonical
/// product (bare name, then lowest product id) sources the card's name and
/// rarity; the image falls back to any sibling when the canonical has none.
pub fn synthesize_cards(
    conn: &mut Connection,
    group_id: i64,
    products: &[TcgProduct],
) -> Result<usize> {
    let code = set_code(group_id);

    // Canonical-first: bare-name products before tagged ones, then by
    // product id so the choice is stable across refreshes.
    let mut ordered: Vec<&TcgProduct> = products.iter().filter(|p| is_card(p)).collect();
    ordered.sort_by_key(|p| (p.name.contains('(') || p.name.contains('['), p.product_id));

    let mut by_number: BTreeMap<String, Vec<&TcgProduct>> = BTreeMap::new();
    for p in ordered {
        by_number
            .entry(normalize_collector_number(&collector_number(p)))
            .or_default()
            .push(p);
    }

    let printed_total = derive_printed_total(products);

    let tx = conn.transaction()?;
    let mut n = 0;
    for (number, group) in &by_number {
        let canonical = group[0];
        let image = group.iter().find_map(|p| {
            if p.image_count > 0 {
                p.image_url.as_deref()
            } else {
                None
            }
        });
        let card_id = format!("{code}-{number}");
        let (supertype, subtypes, types) = classify_card_type(
            extended(canonical, "CardType"),
            extended(canonical, "Stage"),
        );
        tx.execute(
            "INSERT INTO cards \
               (card_id, set_code, number, number_sortable, name, supertype, \
                subtypes, hp, types, rarity, flavor_text, attacks, \
                weaknesses, resistances, retreat_cost, image_small, image_large) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16) \
             ON CONFLICT(card_id) DO UPDATE SET \
               set_code        = excluded.set_code, \
               number          = excluded.number, \
               number_sortable = excluded.number_sortable, \
               name            = excluded.name, \
               supertype       = excluded.supertype, \
               subtypes        = excluded.subtypes, \
               hp              = excluded.hp, \
               types           = excluded.types, \
               rarity          = excluded.rarity, \
               flavor_text     = excluded.flavor_text, \
               attacks         = excluded.attacks, \
               weaknesses      = excluded.weaknesses, \
               resistances     = excluded.resistances, \
               retreat_cost    = excluded.retreat_cost, \
               image_small     = excluded.image_small, \
               image_large     = excluded.image_large",
            rusqlite::params![
                card_id,
                code,
                number,
                pkdump_core::number_sortable(number),
                pkdump_core::variant::parse_product_card_name(&canonical.name),
                supertype,
                subtypes.map(|v| v.to_string()),
                extended(canonical, "HP").and_then(|h| h.trim().parse::<i64>().ok()),
                types.map(|v| v.to_string()),
                rarity(canonical),
                flavor_text(canonical),
                parse_attacks(canonical).map(|v| v.to_string()),
                parse_weakness_resistance(extended(canonical, "Weakness")).map(|v| v.to_string()),
                parse_weakness_resistance(extended(canonical, "Resistance")).map(|v| v.to_string()),
                parse_retreat_cost(extended(canonical, "Retreat Cost")).map(|v| v.to_string()),
                image,
            ],
        )?;
        n += 1;
    }
    tx.execute(
        "UPDATE sets SET printed_total = ?2, total = ?3 WHERE set_code = ?1",
        rusqlite::params![code, printed_total, by_number.len() as i64],
    )?;
    tx.commit()?;
    Ok(n)
}

/// The set's printed card count, read off the `/total` half of its
/// collector numbers (`"103/081"` → 81). The modal denominator wins;
/// secret rares print a number above it, which is exactly what
/// `printed_total` means everywhere else in the catalog. `None` for the
/// vintage groups whose products carry no numbers at all, and for promo
/// groups whose "total" half is a set code (`"099/SM-P"`).
fn derive_printed_total(products: &[TcgProduct]) -> Option<i64> {
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for p in products.iter().filter(|p| is_card(p)) {
        let Some(raw) = extended(p, "Number") else {
            continue;
        };
        let Some((_, total)) = raw.split_once('/') else {
            continue;
        };
        if let Ok(v) = total.trim().parse::<i64>() {
            *counts.entry(v).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n >= 2)
        .max_by_key(|(total, n)| (*n, *total))
        .map(|(total, _)| total)
}

// ---------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------

/// What one full pass over the Japanese catalog wrote.
#[derive(Debug, Default, Clone, Copy)]
pub struct JapanStats {
    pub groups: usize,
    pub card_products: usize,
    pub sealed_products: usize,
    pub cards: usize,
    pub price_rows: usize,
}

/// Fetch and import the whole Pokémon Japan catalog: groups → sets,
/// products → cards + sealed, and a price snapshot. Shared by
/// `pkdump setup` and `pkdump data refresh`.
///
/// Japanese responses land under the same `source=tcgcsv` prefixes as the
/// English ones — same host, same endpoints, same shape. The `categoryId`
/// that tells them apart is in each part's recorded URL.
///
/// Variant expansion is *not* run here — it runs once over the whole
/// catalog (English and Japanese together) after both categories are in.
pub fn import_all(
    conn: &mut Connection,
    now: &str,
    observed: &str,
    wire: crate::landing::Wire,
) -> Result<JapanStats> {
    use std::io::Write;

    let client = TcgcsvClient::for_category(CATEGORY_POKEMON_JAPAN)?.on_wire(wire);
    let groups = client.fetch_groups()?;
    let mut stats = JapanStats {
        groups: import_groups(conn, &groups, now)?,
        ..Default::default()
    };

    // ~450 groups × 2 requests. Rust block-buffers stdout to a pipe, so
    // flush each progress line or seed.sh's `tee` shows nothing for the
    // whole pass.
    const PROGRESS_EVERY: usize = 50;
    let total = groups.len();
    for (i, group) in groups.iter().enumerate() {
        let products = client.fetch_products(group.group_id)?;
        let (cards, sealed) = import_products(conn, &products, now)?;
        stats.card_products += cards;
        stats.sealed_products += sealed;
        stats.cards += synthesize_cards(conn, group.group_id, &products)?;
        let prices = client.fetch_prices(group.group_id)?;
        stats.price_rows += crate::tcgcsv::import_prices(conn, &prices, observed)?;

        let done = i + 1;
        if done % PROGRESS_EVERY == 0 || done == total {
            println!("  [{done:>4}/{total}] {} cards synthesized", stats.cards);
            let _ = std::io::stdout().flush();
        }
    }
    Ok(stats)
}

// ---------------------------------------------------------------------
// extendedData parsing
// ---------------------------------------------------------------------

/// One `extendedData` value by name, trimmed, `None` when absent or blank.
fn extended<'a>(product: &'a TcgProduct, name: &str) -> Option<&'a str> {
    product
        .extended_data
        .iter()
        .find(|e: &&ExtendedDatum| e.name.eq_ignore_ascii_case(name))
        .map(|e| e.value.trim())
        .filter(|v| !v.is_empty())
}

/// The product's rarity. TCGCSV spells "this card has no rarity symbol"
/// as the literal string `"None"` (8k Japanese products, mostly vintage
/// and deck-exclusive cards); that's an absent rarity, not a tier.
fn rarity(product: &TcgProduct) -> Option<String> {
    extended(product, "Rarity")
        .filter(|r| !r.eq_ignore_ascii_case("None"))
        .map(str::to_string)
}

/// Flavour text: the Pokémon dex blurb where TCGCSV supplies one, else the
/// Trainer/Energy rules text it files under "Description".
fn flavor_text(product: &TcgProduct) -> Option<String> {
    extended(product, "Flavor Text")
        .or_else(|| extended(product, "Description"))
        .map(strip_html)
        .filter(|s| !s.is_empty())
}

/// Split TCGCSV's `CardType` (+ `Stage`) into the catalog's
/// (`supertype`, `subtypes`, `types`) triple.
///
/// `"Trainer - Item"` → (`Trainer`, `["Item"]`, none);
/// `"Basic Energy"` → (`Energy`, `["Basic"]`, none);
/// `"Grass"` + stage `"Stage 1"` → (`Pokémon`, `["Stage 1"]`, `["Grass"]`).
/// TCGCSV occasionally doubles a value (`"Dragon;Dragon"`), so the
/// semicolon-separated list is de-duplicated.
fn classify_card_type(
    card_type: Option<&str>,
    stage: Option<&str>,
) -> (Option<String>, Option<Value>, Option<Value>) {
    let Some(raw) = card_type else {
        return (None, None, None);
    };
    if let Some(tail) = raw.strip_prefix("Trainer") {
        let subtypes = split_list(tail.trim_start_matches([' ', '-']));
        return (Some("Trainer".into()), list_value(subtypes), None);
    }
    if raw.ends_with("Energy") || raw == "Special" {
        let kind = raw.trim_end_matches("Energy").trim();
        let kind = if kind.is_empty() { "Basic" } else { kind };
        return (Some("Energy".into()), Some(json!([kind])), None);
    }
    (
        Some("Pokémon".into()),
        list_value(split_list(stage.unwrap_or_default())),
        list_value(split_list(raw)),
    )
}

/// Split a `;`-separated TCGCSV list, trimming and de-duplicating.
fn split_list(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split(';') {
        let part = part.trim();
        if !part.is_empty() && !out.iter().any(|p| p == part) {
            out.push(part.to_string());
        }
    }
    out
}

fn list_value(items: Vec<String>) -> Option<Value> {
    if items.is_empty() {
        None
    } else {
        Some(Value::from(items))
    }
}

/// The card's attacks, in the `[{name, cost, damage, text}]` shape the
/// card-detail view renders. TCGCSV files them one per extendedData entry
/// (`"Attack 1"` … `"Attack 4"`).
fn parse_attacks(product: &TcgProduct) -> Option<Value> {
    let attacks: Vec<Value> = (1..=4)
        .filter_map(|i| extended(product, &format!("Attack {i}")))
        .filter_map(parse_attack)
        .collect();
    if attacks.is_empty() {
        None
    } else {
        Some(Value::from(attacks))
    }
}

/// Parse one TCGCSV attack string.
///
/// `"[Grass][Colorless] Wrap (20)<br> Flip a coin. …"` →
/// `{"cost": ["Grass", "Colorless"], "name": "Wrap", "damage": "20",
///   "text": "Flip a coin. …"}`. The damage parenthetical is optional and
/// carries the usual modifiers (`"10x"`, `"10+"`); a trailing paren that
/// isn't a damage expression stays part of the name.
fn parse_attack(raw: &str) -> Option<Value> {
    let (head, tail) = match raw.find("<br") {
        Some(i) => (&raw[..i], &raw[i..]),
        None => (raw, ""),
    };

    // Leading "[Energy]" tokens are the attack's cost.
    let mut cost: Vec<String> = Vec::new();
    let mut rest = head.trim_start();
    while let Some(inner) = rest.strip_prefix('[') {
        let Some(close) = inner.find(']') else { break };
        cost.push(inner[..close].trim().to_string());
        rest = inner[close + 1..].trim_start();
    }

    let mut name = rest.trim();
    let mut damage: Option<&str> = None;
    if let Some(open) = name.rfind('(')
        && name.ends_with(')')
    {
        let inner = name[open + 1..name.len() - 1].trim();
        if is_damage(inner) {
            damage = Some(inner);
            name = name[..open].trim();
        }
    }

    let text = strip_html(tail);
    if name.is_empty() && cost.is_empty() && text.is_empty() {
        return None;
    }
    let mut attack = serde_json::Map::new();
    if !cost.is_empty() {
        attack.insert("cost".into(), Value::from(cost));
    }
    if !name.is_empty() {
        attack.insert("name".into(), Value::from(name));
    }
    if let Some(d) = damage {
        attack.insert("damage".into(), Value::from(d));
    }
    if !text.is_empty() {
        attack.insert("text".into(), Value::from(text));
    }
    Some(Value::Object(attack))
}

/// Whether a parenthetical is a damage expression (`"20"`, `"10x"`,
/// `"10+"`, `"30-"`) rather than part of the attack's name.
fn is_damage(inner: &str) -> bool {
    let core = inner.trim_end_matches(['x', 'X', '×', '+', '-', '?']);
    !core.is_empty() && core.chars().all(|c| c.is_ascii_digit())
}

/// Parse a `Weakness` / `Resistance` value into the catalog's
/// `[{type, value}]` shape. TCGCSV writes them as an energy type glued to
/// a modifier, with or without a space: `"Psychic x2"`, `"Darknessx2"`,
/// `"Fighting-30"`. The split point is the modifier, so no list of energy
/// names has to be maintained here.
fn parse_weakness_resistance(raw: Option<&str>) -> Option<Value> {
    let chars: Vec<char> = raw?.chars().collect();
    // No energy type contains a digit or one of these modifier glyphs, so
    // the first one of either marks where the type ends.
    let split = chars
        .iter()
        .position(|c| matches!(c, 'x' | 'X' | '×' | '+' | '-') || c.is_ascii_digit())?;
    let kind: String = chars[..split].iter().collect();
    let value: String = chars[split..].iter().collect();
    let kind = kind.trim();
    let value = value.trim().replace(['x', 'X'], "×");
    if kind.is_empty() || value.is_empty() {
        return None;
    }
    Some(json!([{ "type": kind, "value": value }]))
}

/// Expand TCGCSV's numeric retreat cost into the catalog's energy-symbol
/// array — the card-detail view counts the entries and renders one glyph
/// each, so `"2"` becomes two Colorless symbols.
fn parse_retreat_cost(raw: Option<&str>) -> Option<Value> {
    let n: usize = raw?.trim().parse().ok()?;
    if n == 0 {
        return None;
    }
    Some(Value::from(vec!["Colorless"; n]))
}

/// Flatten TCGCSV's HTML-bearing rules text to the plain string the card
/// detail view renders. `<br>` becomes a space; every other tag is
/// dropped without one (so `<em>(reminder)</em>.` keeps its full stop
/// tight against the paren); the handful of entities TCGCSV emits are
/// decoded.
fn strip_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            rest = "";
            break;
        };
        if rest[open + 1..open + close]
            .trim_start_matches('/')
            .to_ascii_lowercase()
            .starts_with("br")
        {
            out.push(' ');
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    let out = out
        // TCGCSV's vintage Japanese rules text went through a lossy
        // transcode upstream: every "é" is a literal '?' (0x3F), so the
        // WotC-era attack text reads "the Defending Pok?mon is now
        // Poisoned". It is the only recurring casualty across the
        // catalog, and "Pok?mon" is never a legitimate string, so the
        // repair is unambiguous.
        .replace("Pok?mon", "Pokémon")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn product(name: &str, id: i64, ext: &[(&str, &str)]) -> TcgProduct {
        product_in(23723, name, id, ext)
    }

    fn product_in(group_id: i64, name: &str, id: i64, ext: &[(&str, &str)]) -> TcgProduct {
        TcgProduct {
            product_id: id,
            group_id,
            name: name.into(),
            image_url: Some(format!("https://tcgplayer.example/{id}.jpg")),
            url: None,
            image_count: 1,
            extended_data: ext
                .iter()
                .map(|(n, v)| ExtendedDatum {
                    name: (*n).into(),
                    value: (*v).into(),
                })
                .collect(),
        }
    }

    fn shared_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn series_buckets_come_from_the_seed() {
        let m = SeriesMap::load().unwrap();
        assert_eq!(
            m.series_for("Mystery of the Fossils", Some("1997/06/21")),
            "Pokémon JP — Original Era"
        );
        assert_eq!(
            m.series_for("Gold, Silver, to a New World...", Some("2000/02/04")),
            "Pokémon JP — Neo Era"
        );
        assert_eq!(
            m.series_for("Base Expansion Pack", Some("2001/12/01")),
            "Pokémon JP — VS & e-Card Era"
        );
        assert_eq!(
            m.series_for("SV9: Battle Partners", Some("2025/01/24")),
            "Pokémon JP — Scarlet & Violet Era"
        );
        assert_eq!(
            m.series_for("M5: Abyss Eye", Some("2026/05/22")),
            "Pokémon JP — Mega Evolution Era"
        );
        // Undated promo groups get their own bucket rather than being
        // filed under whatever era the crawl happened to run in.
        assert_eq!(
            m.series_for("Vending Machine cards Series 1 (Blue)", None),
            "Pokémon JP — Undated Promos"
        );
        // A name override beats the date rule.
        assert_eq!(
            m.series_for("Scarlet & Violet Unnumbered Energies", Some("2025/08/01")),
            "Pokémon JP — Scarlet & Violet Era"
        );
    }

    #[test]
    fn release_date_rejects_the_undated_placeholder() {
        assert_eq!(
            release_date(Some("2026-05-22T00:00:00")).as_deref(),
            Some("2026/05/22")
        );
        // TCGCSV stamps undated groups with the response time.
        assert_eq!(release_date(Some("2026-07-30T20:16:11.0447291Z")), None);
        assert_eq!(release_date(None), None);
    }

    #[test]
    fn vintage_cards_without_numbers_are_still_cards() {
        // Mystery of the Fossils (group 23723) ships no `Number` at all.
        // The English `Number`-based discriminator would file every one
        // of these as a sealed product.
        let ekans = product(
            "Ekans",
            575663,
            &[("Rarity", "Common"), ("CardType", "Grass"), ("HP", "40")],
        );
        let sealed = product(
            "Limited Collection Master Battle Set",
            657209,
            &[("Description", "What is in the box: …")],
        );
        assert!(is_card(&ekans));
        assert!(!is_card(&sealed));
        assert_eq!(collector_number(&ekans), "p575663");
    }

    #[test]
    fn attack_string_parses_into_cost_name_damage_text() {
        let v = parse_attack(
            "[Grass][Colorless] Wrap (20)<br> Flip a coin. If heads, the \
             Defending Pokemon is now Paralyzed.",
        )
        .unwrap();
        assert_eq!(v["cost"], json!(["Grass", "Colorless"]));
        assert_eq!(v["name"], json!("Wrap"));
        assert_eq!(v["damage"], json!("20"));
        assert_eq!(
            v["text"],
            json!("Flip a coin. If heads, the Defending Pokemon is now Paralyzed.")
        );

        // No damage, no text.
        let v = parse_attack("[Colorless] Scratch (10)").unwrap();
        assert_eq!(v["damage"], json!("10"));
        assert_eq!(v.get("text"), None);

        // Damage modifiers stay attached to the number.
        let v = parse_attack("[Fighting][Colorless] Stone Barrage (10x)<br> Flip a coin.").unwrap();
        assert_eq!(v["damage"], json!("10x"));
        let v = parse_attack("[Water] Water Gun (10+)<br> Does 10 damage plus …").unwrap();
        assert_eq!(v["damage"], json!("10+"));

        // An attack with no damage keeps its whole name.
        let v = parse_attack("[Water] Call for Family<br> Search your deck …").unwrap();
        assert_eq!(v["name"], json!("Call for Family"));
        assert_eq!(v.get("damage"), None);

        // `<em>` emphasis inside the rules text is flattened away.
        let v = parse_attack(
            "[Grass] Minimize<br> All damage is reduced by 20 \
             <em>(after applying Weakness and Resistance)</em>.",
        )
        .unwrap();
        assert_eq!(
            v["text"],
            json!("All damage is reduced by 20 (after applying Weakness and Resistance).")
        );
    }

    #[test]
    fn weakness_and_resistance_split_without_an_energy_list() {
        assert_eq!(
            parse_weakness_resistance(Some("Psychic x2")).unwrap(),
            json!([{ "type": "Psychic", "value": "×2" }])
        );
        // TCGCSV drops the space on roughly half the rows.
        assert_eq!(
            parse_weakness_resistance(Some("Darknessx2")).unwrap(),
            json!([{ "type": "Darkness", "value": "×2" }])
        );
        assert_eq!(
            parse_weakness_resistance(Some("Fighting-30")).unwrap(),
            json!([{ "type": "Fighting", "value": "-30" }])
        );
        assert_eq!(
            parse_weakness_resistance(Some("Lightning -30")).unwrap(),
            json!([{ "type": "Lightning", "value": "-30" }])
        );
        assert_eq!(parse_weakness_resistance(None), None);
    }

    #[test]
    fn retreat_cost_expands_to_energy_symbols() {
        assert_eq!(
            parse_retreat_cost(Some("2")).unwrap(),
            json!(["Colorless", "Colorless"])
        );
        assert_eq!(parse_retreat_cost(Some("0")), None);
        assert_eq!(parse_retreat_cost(None), None);
    }

    #[test]
    fn card_type_splits_into_supertype_subtypes_and_types() {
        let (s, sub, ty) = classify_card_type(Some("Trainer - Item"), None);
        assert_eq!(s.as_deref(), Some("Trainer"));
        assert_eq!(sub, Some(json!(["Item"])));
        assert_eq!(ty, None);

        let (s, sub, ty) = classify_card_type(Some("Trainer"), None);
        assert_eq!(s.as_deref(), Some("Trainer"));
        assert_eq!(sub, None);
        assert_eq!(ty, None);

        let (s, sub, ty) = classify_card_type(Some("Basic Energy"), None);
        assert_eq!(s.as_deref(), Some("Energy"));
        assert_eq!(sub, Some(json!(["Basic"])));
        assert_eq!(ty, None);

        let (s, sub, ty) = classify_card_type(Some("Special Energy"), None);
        assert_eq!(s.as_deref(), Some("Energy"));
        assert_eq!(sub, Some(json!(["Special"])));
        assert_eq!(ty, None);

        let (s, sub, ty) = classify_card_type(Some("Grass"), Some("Stage 1"));
        assert_eq!(s.as_deref(), Some("Pokémon"));
        assert_eq!(sub, Some(json!(["Stage 1"])));
        assert_eq!(ty, Some(json!(["Grass"])));

        // TCGCSV occasionally doubles a value.
        let (_, _, ty) = classify_card_type(Some("Dragon;Dragon"), Some("Basic"));
        assert_eq!(ty, Some(json!(["Dragon"])));
    }

    #[test]
    fn import_groups_synthesizes_sets_without_touching_the_english_catalog() {
        // "Pokemon Jungle" (JP group 23722) normalizes to the same name
        // as the English `base2` set. The English auto-linker would hand
        // it that set_code; the JP path must build its own.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('base2', 'JU', 'Jungle', 'Base', '1999/06/16')",
            [],
        )
        .unwrap();
        let groups = vec![
            TcgGroup {
                group_id: 23722,
                name: "Pokemon Jungle".into(),
                abbreviation: Some(String::new()),
                published_on: Some("1997-03-05T00:00:00".into()),
            },
            TcgGroup {
                group_id: 24711,
                name: "M5: Abyss Eye".into(),
                abbreviation: Some("M5".into()),
                published_on: Some("2026-05-22T00:00:00".into()),
            },
        ];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();

        let (name, series, released): (String, String, Option<String>) = conn
            .query_row(
                "SELECT name, series, release_date FROM sets WHERE set_code = 'jp-23722'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(name, "Pokemon Jungle");
        assert_eq!(series, "Pokémon JP — Original Era");
        assert_eq!(released.as_deref(), Some("1997/03/05"));

        // base2 is untouched — no JP group claimed it.
        let base2_groups: i64 = conn
            .query_row(
                "SELECT count(*) FROM tcgplayer_groups WHERE set_code = 'base2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(base2_groups, 0);

        // Empty abbreviations are stored as NULL, not "".
        let ptcgo: Option<String> = conn
            .query_row(
                "SELECT ptcgo_code FROM sets WHERE set_code = 'jp-23722'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ptcgo, None);
        let ptcgo: Option<String> = conn
            .query_row(
                "SELECT ptcgo_code FROM sets WHERE set_code = 'jp-24711'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ptcgo.as_deref(), Some("M5"));

        // Idempotent.
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();
        let sets: i64 = conn
            .query_row(
                "SELECT count(*) FROM sets WHERE set_code LIKE 'jp-%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sets, 2);
    }

    #[test]
    fn synthesize_cards_builds_rows_from_extended_data() {
        let (_d, mut conn) = shared_db();
        let groups = vec![TcgGroup {
            group_id: 23723,
            name: "Mystery of the Fossils".into(),
            abbreviation: Some(String::new()),
            published_on: Some("1997-06-21T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();

        let products = vec![
            product(
                "Ekans",
                575663,
                &[
                    ("Rarity", "Common"),
                    ("CardType", "Grass"),
                    ("HP", "40"),
                    ("Stage", "Basic"),
                    ("Weakness", "Psychic x2"),
                    ("Retreat Cost", "1"),
                    (
                        "Attack 1",
                        "[Grass] Spit Poison<br> Flip a coin. If heads, the \
                         Defending Pokemon is now Poisoned.",
                    ),
                ],
            ),
            product(
                "Energy Search",
                575664,
                &[
                    ("Rarity", "Common"),
                    ("CardType", "Trainer - Item"),
                    ("Description", "Search your deck for a Basic Energy card."),
                ],
            ),
            // Sealed — no CardType.
            product("Fossil Booster Box", 999999, &[("Description", "36 packs")]),
        ];
        import_products(&mut conn, &products, "2026-07-31").unwrap();
        let n = synthesize_cards(&mut conn, 23723, &products).unwrap();
        assert_eq!(n, 2, "two cards, one sealed product");

        // (name, supertype, hp, types, subtypes, rarity, retreat_cost).
        type SynthesizedCard = (
            String,
            String,
            Option<i64>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let (name, supertype, hp, types, subtypes, rarity, retreat): SynthesizedCard = conn
            .query_row(
                "SELECT name, supertype, hp, types, subtypes, rarity, retreat_cost \
                   FROM cards WHERE card_id = 'jp-23723-p575663'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(name, "Ekans");
        assert_eq!(supertype, "Pokémon");
        assert_eq!(hp, Some(40));
        assert_eq!(types.as_deref(), Some(r#"["Grass"]"#));
        assert_eq!(subtypes.as_deref(), Some(r#"["Basic"]"#));
        assert_eq!(rarity.as_deref(), Some("Common"));
        assert_eq!(retreat.as_deref(), Some(r#"["Colorless"]"#));

        // The Trainer's rules text lands in flavor_text.
        let flavor: Option<String> = conn
            .query_row(
                "SELECT flavor_text FROM cards WHERE card_id = 'jp-23723-p575664'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            flavor.as_deref(),
            Some("Search your deck for a Basic Energy card.")
        );

        // The sealed product is filed as sealed, not as a card.
        let sealed: i64 = conn
            .query_row(
                "SELECT count(*) FROM sealed_products WHERE product_id = 999999",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sealed, 1);

        // Re-running is idempotent.
        synthesize_cards(&mut conn, 23723, &products).unwrap();
        let cards: i64 = conn
            .query_row(
                "SELECT count(*) FROM cards WHERE set_code = 'jp-23723'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cards, 2);
    }

    #[test]
    fn printings_of_one_card_collapse_onto_a_single_slot() {
        // TCGCSV lists the Mirror Holofoil printing as its own product
        // sharing the base card's number. That's a printing, not a
        // second binder slot — and its name-derived variant has to be
        // recorded so variant expansion doesn't collide it with the
        // base product's holo.
        let (_d, mut conn) = shared_db();
        let groups = vec![TcgGroup {
            group_id: 24711,
            name: "M5: Abyss Eye".into(),
            abbreviation: Some("M5".into()),
            published_on: Some("2026-05-22T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();
        let products = vec![
            product_in(
                24711,
                "Oddish - 001/081",
                700001,
                &[
                    ("Number", "001/081"),
                    ("Rarity", "Common"),
                    ("CardType", "Grass"),
                ],
            ),
            product_in(
                24711,
                "Oddish - 001/081 (Mirror Holofoil)",
                700002,
                &[
                    ("Number", "001/081"),
                    ("Rarity", "Common"),
                    ("CardType", "Grass"),
                ],
            ),
        ];
        import_products(&mut conn, &products, "2026-07-31").unwrap();
        let n = synthesize_cards(&mut conn, 24711, &products).unwrap();
        assert_eq!(n, 1, "both products are the same card");

        let derived: Vec<Option<String>> = {
            let mut stmt = conn
                .prepare(
                    "SELECT derived_variant FROM tcgcsv_products \
                      WHERE group_id = 24711 ORDER BY product_id",
                )
                .unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(derived, vec![None, Some("mirror_holo".to_string())]);

        // The card is filed at the printed number, not the product id.
        let number: String = conn
            .query_row(
                "SELECT number FROM cards WHERE set_code = 'jp-24711'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(number, "1");
    }

    #[test]
    fn japanese_cards_expand_into_printings_end_to_end() {
        // The whole path a Japanese card takes: group → set, product →
        // card, price → printing. Two things this pins down that unit
        // tests can't: the `jp-<group_id>` set_code reaches
        // `variants_from_tcgcsv` through the `tcgplayer_groups` bridge,
        // and the unsuffixed "1st Edition" / "Unlimited" sub_types the
        // Japanese catalog prices under resolve through the global
        // default map (they had no entry there before Japan; every
        // printing under them was silently dropped).
        let (_d, mut conn) = shared_db();
        let groups = vec![TcgGroup {
            group_id: 23723,
            name: "Mystery of the Fossils".into(),
            abbreviation: Some(String::new()),
            published_on: Some("1997-06-21T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();

        let products = vec![product(
            "Ekans",
            575663,
            &[("Rarity", "Common"), ("CardType", "Grass"), ("HP", "40")],
        )];
        import_products(&mut conn, &products, "2026-07-31").unwrap();
        synthesize_cards(&mut conn, 23723, &products).unwrap();

        let prices: Vec<crate::tcgcsv::TcgPrice> =
            ["Normal", "Holofoil", "1st Edition", "Unlimited"]
                .iter()
                .map(|sub| crate::tcgcsv::TcgPrice {
                    product_id: 575663,
                    sub_type_name: Some((*sub).into()),
                    low_price: Some(0.39),
                    mid_price: Some(0.77),
                    high_price: Some(14.9),
                    market_price: Some(0.85),
                    direct_low_price: None,
                })
                .collect();
        crate::tcgcsv::import_prices(&mut conn, &prices, "2026-07-31").unwrap();

        let overlay = crate::overrides::load_variant_augmentations().unwrap();
        crate::overrides::expand_all_printings(&mut conn, &overlay).unwrap();

        let variants: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT variant FROM printings \
                      WHERE card_id = 'jp-23723-p575663' AND deprecated_at IS NULL \
                      ORDER BY variant",
                )
                .unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(
            variants,
            vec![
                "first_ed_normal".to_string(),
                "holo".to_string(),
                "normal".to_string(),
                "unlimited_normal".to_string(),
            ],
            "every advertised sub_type must resolve to a printing"
        );

        // Each printing carries the product id so the price join stays
        // a plain JOIN.
        let product_ids: i64 = conn
            .query_row(
                "SELECT count(*) FROM printings \
                  WHERE card_id = 'jp-23723-p575663' AND tcgplayer_product_id = 575663",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(product_ids, 4);
    }

    #[test]
    fn printed_total_comes_from_the_modal_denominator() {
        let products = vec![
            product(
                "A - 001/081",
                1,
                &[("Number", "001/081"), ("CardType", "Grass")],
            ),
            product(
                "B - 002/081",
                2,
                &[("Number", "002/081"), ("CardType", "Fire")],
            ),
            // Secret rare printed above the set total.
            product(
                "C - 103/081",
                3,
                &[("Number", "103/081"), ("CardType", "Trainer")],
            ),
        ];
        assert_eq!(derive_printed_total(&products), Some(81));

        // Promo groups number against a set code, not a total.
        let promos = vec![
            product(
                "D - 001/SM-P",
                4,
                &[("Number", "001/SM-P"), ("CardType", "Grass")],
            ),
            product(
                "E - 002/SM-P",
                5,
                &[("Number", "002/SM-P"), ("CardType", "Fire")],
            ),
        ];
        assert_eq!(derive_printed_total(&promos), None);

        // Vintage groups have no numbers at all.
        let vintage = vec![product("Ekans", 6, &[("CardType", "Grass")])];
        assert_eq!(derive_printed_total(&vintage), None);
    }
}
