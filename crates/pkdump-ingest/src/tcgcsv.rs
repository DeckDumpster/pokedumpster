//! Importer for TCGCSV (`tcgcsv.com`) — the daily TCGplayer bulk dump.
//!
//! Provides set ("group") metadata, the sealed-product catalog, and spot
//! prices (RESEARCH.md §2.5). categoryId 3 is English Pokémon and 85 is
//! Pokémon Japan; the client is parameterized over the category so both
//! flow through this one endpoint layer. No auth, no rate limit.
//! PokeDumpster snapshots prices daily into a time series.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use pkdump_lake::{Dataset, PartFormat, RawLanding, Source};
use rusqlite::Connection;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{IngestError, Result};
use crate::landing::{self, Landing};

const BASE_URL: &str = "https://tcgcsv.com/tcgplayer";

/// TCGplayer's category id for English Pokémon — the catalog every
/// pokemontcg.io-backed set bridges against.
pub const CATEGORY_POKEMON: i64 = 3;

/// TCGplayer's category id for Pokémon Japan. It has no pokemontcg.io
/// counterpart, so its sets and cards are synthesized from TCGCSV alone —
/// see `crate::japan`.
pub const CATEGORY_POKEMON_JAPAN: i64 = 85;

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

/// The first `<digits>/<digits>` fraction in a product name, e.g.
/// `"Buck's Training - 130/146 (Prerelease)"` → `("130", "146")`. `None`
/// when the name carries no such fraction.
fn name_number_fraction(name: &str) -> Option<(&str, &str)> {
    let b = name.as_bytes();
    for slash in 0..b.len() {
        if b[slash] != b'/' {
            continue;
        }
        let mut start = slash;
        while start > 0 && b[start - 1].is_ascii_digit() {
            start -= 1;
        }
        let mut end = slash + 1;
        while end < b.len() && b[end].is_ascii_digit() {
            end += 1;
        }
        // Both halves are ASCII digits, so these are char boundaries.
        if start < slash && end > slash + 1 {
            return Some((&name[start..slash], &name[slash + 1..end]));
        }
    }
    None
}

/// Recover a `/total` suffix that TCGplayer's `extendedData` "Number"
/// dropped but the product *name* still spells out.
///
/// TCGCSV normally ships the printed form in "Number" (`"130/146"`), and
/// cross-group promo resolution leans on the `/total` half to decide which
/// set's card 130 a stamped promo belongs to — see the `set_total` gate in
/// `overrides::expand_all_printings`. Two MCAP (group 2374) products carry
/// a bare `"130"` instead: `"Buck's Training - 130/146 (Prerelease)"`
/// (221176) and its `[Staff]` sibling (532631). With no total to match on,
/// and no promo namespace to fall back to (the number is pure digits),
/// both stayed unmodeled after the MCAP epic. They are the only two
/// products with this shape across every group we ingest.
///
/// The name is the fallback source of truth, but only when its fraction
/// starts with the very number upstream gave us — a fraction that
/// disagrees belongs to some other card and must not be grafted on.
fn restore_truncated_set_total(number: &str, name: &str) -> Option<String> {
    if number.contains('/') {
        return None;
    }
    let (num, total) = name_number_fraction(name)?;
    (normalize_collector_number(num) == normalize_collector_number(number))
        .then(|| format!("{num}/{total}"))
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
    /// What this bridge represents in the set ↔ groups mapping. 'primary'
    /// (the default) is the regular print run — bridging it wipes any
    /// auto-link to the same set_code so the bridge wins on conflict.
    /// Auxiliary roles like 'shadowless' or 'first_edition' don't wipe;
    /// they layer additional (group, set) bridges on top of the primary,
    /// which is what makes Base Set's group-604 + group-1663 split
    /// expressible in the data model (pokedumpster-5is).
    #[serde(default = "default_bridge_role")]
    role: String,
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
    /// Optional overrides for the set's logo/symbol URLs. When present,
    /// these UPDATE the existing row even for upstream-managed sets —
    /// "the bridge knows better than upstream art." Used to swap
    /// pokemontcg.io's busy SVP symbol for a self-hosted glyph that
    /// matches the rest of the promo-set lineup.
    #[serde(default)]
    logo_url: Option<String>,
    #[serde(default)]
    symbol_url: Option<String>,
    /// When set, every synthesized card in this bridge takes this
    /// literal rarity regardless of what the canonical TCGCSV product's
    /// "Rarity" extendedData says. Used for promo sets where the
    /// convention (matching pokemontcg.io's svp) is that every card
    /// reads as "Promo" — TCGCSV otherwise tags chase cards as e.g.
    /// "Illustration Rare", which the promo-set binder shouldn't show.
    #[serde(default)]
    synthesize_rarity: Option<String>,
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
}

const SET_BRIDGES_JSON: &str = include_str!("../../../data/overrides/tcgcsv_set_bridges.json");

fn default_bridge_role() -> String {
    "primary".to_string()
}

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
/// One set is claimed by at most one group via auto-link (the regular
/// print run, role='primary'); auxiliary print runs that need a second
/// group bridged to the same set (e.g. base1's Shadowless group 1663)
/// are supplied by the bridge overlay with role='shadowless' and don't
/// pass through this function. `ptcgo_code` is not unique (promo codes
/// recur, many are NULL) and TCGCSV reuses a name across the odd group,
/// so any tier may offer the same set to several groups — the first
/// group (by id, deterministic) takes it. Groups are processed in id
/// order so the assignment is stable across re-runs.
///
/// The Japanese catalog is excluded from both tiers. Only English
/// (category 3) groups reach this function — `japan::import_groups` owns
/// the category-85 bridge — but `sets` holds last night's `jp-` rows by
/// the time a refresh runs the English pass, and Japanese sets carry the
/// same abbreviations and names their English counterparts do: "SM06"
/// (`jp-23685`, "SM6: Forbidden Light") is the ptcgo_code of no English
/// set, so English group 2209 ("SM - Forbidden Light", abbreviation
/// "SM06") would take the Japanese set on tier 1 and never reach the
/// tier-2 name match that bridges it correctly. 19 English groups
/// resolve onto a `jp-` set without this filter.
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
              WHERE set_code NOT LIKE ?1 || '%' \
              ORDER BY release_date, set_code",
        )?;
        let rows = stmt.query_map([crate::japan::SET_CODE_PREFIX], |r| {
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
/// [`resolve_group_set_links`]. `tcgplayer_groups.set_code` is the 1:N bridge
/// from set → groups; each row also carries a `role` (`primary` for the
/// regular run, `shadowless`/`first_edition`/… for auxiliary print runs
/// supplied by the bridge overlay). Returns the number of groups.
///
/// Before the auto-link runs, the bridge overlay
/// (`data/overrides/tcgcsv_set_bridges.json`) is applied: it synthesizes
/// any `sets` rows the overlay declares (idempotent INSERT-OR-IGNORE) so
/// the auto-linker can see them, then injects (group_id → set_code)
/// entries that take precedence over abbreviation/name matching. A
/// `primary`-role bridge wipes any auto-link to the same set_code and
/// claims the slot; auxiliary-role bridges layer additional links on
/// top — the auto-linked primary stays intact.
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
                        b.logo_url,
                        b.symbol_url,
                    ],
                )?;
                // Heal a synth-owned set row whose synthesize block has
                // grown new fields since it was first inserted.
                // `ptcgio_fetched_at IS NULL` scopes this to synth rows
                // so upstream-managed sets stay untouched here — the
                // logo/symbol override below handles those explicitly.
                tx.execute(
                    "UPDATE sets \
                        SET ptcgo_code   = ?2, \
                            name         = ?3, \
                            series       = ?4, \
                            release_date = ?5 \
                      WHERE set_code = ?1 \
                        AND ptcgio_fetched_at IS NULL",
                    rusqlite::params![
                        b.set_code,
                        synth.ptcgo_code,
                        synth.name,
                        synth.series,
                        synth.release_date,
                    ],
                )?;
            }
            // logo_url / symbol_url overrides apply to both synth and
            // upstream-managed rows. The bridge is the source of truth
            // for set art when it asserts an override — used to swap
            // pokemontcg.io's busy SVP symbol for a self-hosted glyph
            // that matches MEP and the rest of the promo lineup.
            if b.logo_url.is_some() || b.symbol_url.is_some() {
                tx.execute(
                    "UPDATE sets \
                        SET logo_url   = COALESCE(?2, logo_url), \
                            symbol_url = COALESCE(?3, symbol_url) \
                      WHERE set_code = ?1",
                    rusqlite::params![b.set_code, b.logo_url, b.symbol_url],
                )?;
            }
        }
        tx.commit()?;
    }

    // (group_id) -> (set_code, role). Start from auto-links (all primary),
    // then layer bridges on top per role.
    let auto = resolve_group_set_links(conn, groups)?;
    let mut links_with_role: std::collections::HashMap<i64, (String, String)> = auto
        .into_iter()
        .map(|(g, s)| (g, (s, "primary".to_string())))
        .collect();
    for b in &bridges {
        if b.role == "primary" {
            // Primary bridges win on conflict — strip any other auto-link
            // claiming the same set_code as primary, then claim it.
            links_with_role.retain(|_, (s, r)| !(s == &b.set_code && r == "primary"));
        }
        links_with_role.insert(b.tcgcsv_group_id, (b.set_code.clone(), b.role.clone()));
    }

    let tx = conn.transaction()?;
    for g in groups {
        let (set_code, role) = match links_with_role.get(&g.group_id) {
            Some((s, r)) => (Some(s.as_str()), r.as_str()),
            None => (None, "primary"),
        };
        tx.execute(
            "INSERT INTO tcgplayer_groups
               (group_id, set_code, name, abbreviation, published_on, fetched_at, role)
             VALUES (?1, ?6, ?2, ?4, ?3, ?5, ?7)
             ON CONFLICT(group_id) DO UPDATE SET
               set_code     = excluded.set_code,
               name         = excluded.name,
               abbreviation = excluded.abbreviation,
               published_on = excluded.published_on,
               fetched_at   = excluded.fetched_at,
               role         = excluded.role",
            rusqlite::params![
                g.group_id,
                g.name,
                g.published_on,
                g.abbreviation,
                now,
                set_code,
                role,
            ],
        )?;
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
        n += synthesize_cards_for_group(
            &tx,
            b.tcgcsv_group_id,
            &b.set_code,
            b.synthesize_rarity.as_deref(),
        )?;
    }
    tx.commit()?;
    Ok(n)
}

/// Build `cards` rows for one TCGCSV group — the body shared by the
/// bridge overlay (`synthesize_cards_for_bridges`) and the auto-discovery
/// path (`crate::set_discovery`). See the doc comment on
/// `synthesize_cards_for_bridges` for the sourcing and healing rules.
///
/// `rarity_override` forces every card's rarity (promo sets), and is what
/// a bridge's `synthesize_rarity` supplies. Returns the number of `cards`
/// rows freshly inserted.
pub(crate) fn synthesize_cards_for_group(
    tx: &Connection,
    group_id: i64,
    set_code: &str,
    rarity_override: Option<&str>,
) -> Result<usize> {
    let mut n = 0;
    // Pull every product in the group with a collector number. Order so
    // bare-name products (no `[`, no `(`) come first — the first row per
    // number becomes the canonical source.
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
        .query_map([group_id], |r| {
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
        // A caller-supplied rarity (e.g. "Promo" for MEP) wins over
        // whatever TCGCSV tags individual chase cards as. Mirrors the
        // pokemontcg.io svp convention where every Black Star Promo
        // card carries rarity "Promo".
        let rarity = rarity_override.or_else(|| products.iter().find_map(|(_, _, r)| r.as_deref()));
        let card_id = format!("{set_code}-{number}");
        let sortable = pkdump_core::number_sortable(&number);

        tx.execute(
            "INSERT OR IGNORE INTO cards \
               (card_id, set_code, number, number_sortable, name, rarity, \
                image_small, image_large) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            rusqlite::params![
                card_id, set_code, number, sortable, card_name, rarity, image
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
        // Upstream occasionally truncates the printed "130/146" down to a
        // bare "130"; the product name still spells the full form.
        let repaired = restore_truncated_set_total(number, &product.name);
        let number = repaired.as_deref().unwrap_or(number);
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

/// A blocking client for the TCGCSV endpoints of one TCGplayer category.
pub struct TcgcsvClient {
    http: reqwest::blocking::Client,
    category_id: i64,
    landing: Landing,
    base_url: String,
}

impl TcgcsvClient {
    /// A client for English Pokémon ([`CATEGORY_POKEMON`]).
    pub fn new() -> Result<Self> {
        Self::for_category(CATEGORY_POKEMON)
    }

    /// A client for an arbitrary TCGplayer category — [`CATEGORY_POKEMON`]
    /// or [`CATEGORY_POKEMON_JAPAN`].
    pub fn for_category(category_id: i64) -> Result<Self> {
        Ok(Self {
            http: reqwest::blocking::Client::builder()
                .user_agent("pokedumpster/0.1 (+cache-population)")
                .timeout(Duration::from_secs(60))
                .build()?,
            category_id,
            landing: None,
            base_url: crate::upstream::base_url(crate::upstream::ENV_TCGCSV_BASE_URL, BASE_URL),
        })
    }

    /// Land every response this client receives in `landing`.
    ///
    /// Without this the client behaves exactly as it did before the landing
    /// zone existed.
    pub fn landing_in(mut self, landing: Arc<RawLanding>) -> Self {
        self.landing = Some(landing);
        self
    }

    /// Point the client at a different origin. Test-tier only — it is how
    /// the landing path is driven against a local server instead of
    /// tcgcsv.com.
    pub fn base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    /// The TCGplayer category this client reads.
    pub fn category_id(&self) -> i64 {
        self.category_id
    }

    fn get(&self, path: &str, dataset: Dataset) -> Result<Value> {
        std::thread::sleep(Duration::from_millis(50));
        let category = self.category_id;
        let base = &self.base_url;
        let body = landing::fetch_bytes(
            &self.http,
            self.http.get(format!("{base}/{category}{path}")),
            self.landing.as_ref(),
            Source::Tcgcsv,
            dataset,
            PartFormat::Json,
        )?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// Every group (set) in this client's category.
    pub fn fetch_groups(&self) -> Result<Vec<TcgGroup>> {
        parse_results(&self.get("/groups", Dataset::Groups)?)
    }

    /// Every product (cards + sealed) in a group.
    pub fn fetch_products(&self, group_id: i64) -> Result<Vec<TcgProduct>> {
        parse_results(&self.get(&format!("/{group_id}/products"), Dataset::Products)?)
    }

    /// Every spot price in a group.
    pub fn fetch_prices(&self, group_id: i64) -> Result<Vec<TcgPrice>> {
        parse_results(&self.get(&format!("/{group_id}/prices"), Dataset::Prices)?)
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
                "SELECT group_id FROM tcgplayer_groups \
                  WHERE set_code = 'sv3pt5' AND role = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(gid, Some(23237));
    }

    #[test]
    fn import_groups_survives_shared_ptcgo_code() {
        // Two sets share a ptcgo_code (real promo codes recur). Two groups
        // carry that code. Each group links a single distinct set as its
        // primary bridge.
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
                "SELECT count(*) FROM tcgplayer_groups \
                  WHERE set_code IS NOT NULL AND role = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 2, "each group linked one distinct set");
        let distinct: i64 = conn
            .query_row(
                "SELECT count(DISTINCT set_code) FROM tcgplayer_groups \
                  WHERE set_code IS NOT NULL AND role = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            distinct, 2,
            "the two sets each have a distinct primary group"
        );
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
            "name fallback must bridge the set"
        );

        // Idempotent across re-runs.
        import_groups(&mut conn, &groups, "2026-05-19").unwrap();
        let again: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 2906",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(again.as_deref(), Some("swsh8"));
    }

    #[test]
    fn import_groups_never_bridges_an_english_group_to_a_japanese_set() {
        // A refresh runs the English TCGCSV pass *before* the Japanese one,
        // but `sets` already holds the previous night's `jp-` rows — and
        // those carry the abbreviations and names their English
        // counterparts do. "SM06" is the ptcgo_code of no English set, so
        // without the `jp-` filter in `resolve_group_set_links` the English
        // "SM - Forbidden Light" group takes the Japanese set on tier 1 and
        // never reaches the tier-2 name match that bridges it correctly —
        // leaving the English set with no group, hence no products and no
        // prices, and the Japanese set with an English group's products.
        // 19 real English groups resolve this way against live TCGCSV.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date) \
             VALUES ('jp-23685', 'SM06', 'SM6: Forbidden Light', \
                     'Pokémon JP — Sun & Moon Era', '2018/04/06'), \
                    ('sm6', 'FLI', 'Forbidden Light', 'Sun & Moon', '2018/05/04')",
            [],
        )
        .unwrap();
        let groups = vec![TcgGroup {
            group_id: 2209,
            name: "SM - Forbidden Light".into(),
            abbreviation: Some("SM06".into()),
            published_on: None,
        }];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();

        let set_code: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 2209",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            set_code.as_deref(),
            Some("sm6"),
            "an English group must bridge to the English set, not the Japanese one"
        );
    }

    #[test]
    fn import_groups_keeps_link_sides_consistent() {
        // Many groups share the "PR" promo abbreviation. Each set ↔ group
        // bridge stored in tcgplayer_groups.set_code is the single source
        // of truth — variant expansion and import_sealed_products both
        // read from it, so there's only one side to keep consistent.
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

        // Two distinct sets, two distinct group ids — each set has one
        // primary bridge and they don't collide.
        let linked: i64 = conn
            .query_row(
                "SELECT count(DISTINCT set_code) FROM tcgplayer_groups \
                  WHERE set_code IS NOT NULL AND role = 'primary'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 2);
        // Each linked group has a set_code that points back to a real set.
        let orphans: i64 = conn
            .query_row(
                "SELECT count(*) FROM tcgplayer_groups g \
                  WHERE g.set_code IS NOT NULL \
                    AND NOT EXISTS \
                      (SELECT 1 FROM sets s WHERE s.set_code = g.set_code)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "every linked group points at a real set");
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

        let set_code: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 22872",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            set_code.as_deref(),
            Some("svp"),
            "bridge links svp to TCGCSV group 22872"
        );
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

        let row: (String, Option<String>, String) = conn
            .query_row(
                "SELECT set_code, ptcgo_code, name FROM sets WHERE set_code = 'mep'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "mep".into(),
                Some("MEP".into()),
                "ME Black Star Promos".into(),
            )
        );
        let bridged: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 24451",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bridged.as_deref(), Some("mep"));
    }

    #[test]
    fn restore_truncated_set_total_only_fires_on_agreeing_name_fraction() {
        // The quirk: upstream "Number" lost the "/146" the name still has.
        assert_eq!(
            restore_truncated_set_total("130", "Buck's Training - 130/146 (Prerelease)").as_deref(),
            Some("130/146")
        );
        assert_eq!(
            restore_truncated_set_total("130", "Buck's Training - 130/146 (Prerelease) [Staff]")
                .as_deref(),
            Some("130/146")
        );
        // Zero-padding differences between the two sources still agree.
        assert_eq!(
            restore_truncated_set_total("7", "Erika's Tangela - 007/217 (Cosmo Holo)").as_deref(),
            Some("007/217")
        );
        // Already complete — nothing to restore.
        assert_eq!(
            restore_truncated_set_total("012/086", "Victini (Black Bolt Stamped)"),
            None
        );
        // No fraction in the name at all (bare promo namespaces).
        assert_eq!(restore_truncated_set_total("060", "Aegislash - 060"), None);
        // A fraction that disagrees with upstream's number belongs to some
        // other card — never graft it on.
        assert_eq!(
            restore_truncated_set_total("130", "Some Promo - 045/094 (Prerelease)"),
            None
        );
        // A lone slash with no digits on one side isn't a fraction.
        assert_eq!(
            restore_truncated_set_total("130", "Mallow & Lana - 256/S-P"),
            None
        );
    }

    #[test]
    fn import_products_recovers_set_total_truncated_by_upstream() {
        // Product 221176: extendedData Number is the bare "130" even though
        // the name spells "130/146". Without the total, cross-group promo
        // resolution can't tell which set's card 130 this is, so the
        // Prerelease promo never attaches to dp6-130. Persist the fuller
        // form the name gives us.
        let (_d, mut conn) = shared_db();
        let products = vec![TcgProduct {
            product_id: 221176,
            group_id: 2374,
            name: "Buck's Training - 130/146 (Prerelease)".into(),
            image_url: None,
            url: None,
            image_count: 1,
            extended_data: vec![
                ExtendedDatum {
                    name: "Number".into(),
                    value: "130".into(),
                },
                ExtendedDatum {
                    name: "Rarity".into(),
                    value: "Promo".into(),
                },
            ],
        }];
        import_products(&mut conn, &products, "2026-07-31").unwrap();
        let number: String = conn
            .query_row(
                "SELECT collector_number FROM tcgcsv_products WHERE product_id = 221176",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(number, "130/146");
        // The repair must not disturb the linking token either side uses.
        assert_eq!(normalize_collector_number(&number), "130");
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
    fn synthesize_rarity_override_in_bridge_wins_over_tcgcsv() {
        // MEP's bridge sets synthesize_rarity="Promo" so every card —
        // including the chase ones TCGCSV tags as "Illustration Rare" /
        // "Holo Rare" — surfaces as Promo, matching the pokemontcg.io
        // svp convention. This test exercises that path via the
        // baked-in MEP bridge entry.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution')",
            [],
        )
        .unwrap();
        let products = vec![TcgProduct {
            product_id: 659231,
            group_id: 24451,
            name: "Meloetta - 026".into(),
            image_url: None,
            url: None,
            image_count: 1,
            extended_data: vec![
                ExtendedDatum {
                    name: "Number".into(),
                    value: "026".into(),
                },
                ExtendedDatum {
                    name: "Rarity".into(),
                    value: "Illustration Rare".into(),
                },
            ],
        }];
        import_products(&mut conn, &products, "2026-05-25").unwrap();
        synthesize_cards_for_bridges(&mut conn).unwrap();
        let rarity: Option<String> = conn
            .query_row(
                "SELECT rarity FROM cards WHERE card_id = 'mep-26'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            rarity.as_deref(),
            Some("Promo"),
            "bridge synthesize_rarity overrides TCGCSV's Illustration Rare tag"
        );
    }

    #[test]
    fn synthesize_cards_writes_rarity_from_canonical_product() {
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution')",
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
            Some("/sets/mep-symbol.svg"),
            "MEP carries a self-hosted symbol so the /browse tile is \
             visually distinct from the regular Mega Evolution series \
             tiles (which all share me1/symbol.png)"
        );
    }

    #[test]
    fn bridge_symbol_url_overrides_upstream_pokemontcg_row() {
        // SVP's pokemontcg.io row carries the official symbol_url, but
        // the bridge ships a self-hosted glyph to match MEP's. The
        // override must replace upstream's symbol on every refresh —
        // unlike name/series, which stay upstream's authority.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date, \
                               symbol_url, ptcgio_fetched_at) \
             VALUES ('svp','PR-SV','Scarlet & Violet Black Star Promos','Scarlet & Violet', \
                     '2023/01/01', \
                     'https://images.pokemontcg.io/svp/symbol.png', \
                     '2026-05-25T00:00:00')",
            [],
        )
        .unwrap();
        let groups = vec![TcgGroup {
            group_id: 22872,
            name: "SV: Scarlet & Violet Promo Cards".into(),
            abbreviation: Some("SVP".into()),
            published_on: Some("2023-03-31T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-05-25").unwrap();

        let row: (String, Option<String>) = conn
            .query_row(
                "SELECT name, symbol_url FROM sets WHERE set_code='svp'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            row.0, "Scarlet & Violet Black Star Promos",
            "name stays under upstream's authority"
        );
        assert_eq!(
            row.1.as_deref(),
            Some("/sets/svp-symbol.svg"),
            "bridge symbol_url overrides the upstream svp/symbol.png"
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
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution')",
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
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution')",
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
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution')",
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
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution')",
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
            "INSERT INTO sets (set_code, ptcgo_code, name, series) \
             VALUES ('mep', 'MEP', 'ME Black Star Promos', 'Mega Evolution')",
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
        let svp_link: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 22872",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(svp_link.as_deref(), Some("svp"));
        // mep was synthesized exactly once, still linked to 24451.
        let mep_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sets WHERE set_code = 'mep'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mep_count, 1, "synthesized set must not duplicate on re-run");
        let mep_link: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 24451",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(mep_link.as_deref(), Some("mep"));
    }
}
