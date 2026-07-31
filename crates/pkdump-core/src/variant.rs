//! Variant codes and the helpers that map between TCGCSV product data and
//! PokeDumpster variant codes.
//!
//! Variants are derived from TCGCSV's product list and price sub_types
//! (PLAN.md §4 — note that the original three-layer scheme has been
//! collapsed: TCGCSV is now authoritative for which printings exist, and
//! the per-card overlay only adds the truly upstream-missing cases like
//! event-stamped promos). See `pkdump-ingest::overrides::expand_all_printings`
//! for the orchestration.

use serde::Deserialize;

/// Variant codes PokeDumpster recognises (RESEARCH.md §4.2). Reference list
/// for documentation; not enforced at runtime — the overlay may legitimately
/// introduce new gimmicks as upstream ships them.
pub const KNOWN_VARIANTS: &[&str] = &[
    // Base treatments — these share one TCGplayer product per card and
    // distinguish themselves via the sub_type. The (group_id, sub_type)
    // → variant mapping lives in shared.sqlite's
    // `tcgcsv_sub_type_variant_map` table (seeded from
    // data/tcgcsv_sub_type_variants.json), not in this crate, because
    // the same sub_type string means different physical printings in
    // different TCGCSV groups (Base Set 604 vs Shadowless 1663).
    "normal",
    "holo",
    "reverse_holo",
    "first_ed_holo",
    "first_ed_normal",
    "unlimited_holo",
    "unlimited_normal",
    "shadowless_normal",
    "shadowless_holo",
    // Pattern overlays — each is its own TCGplayer product. `_rh` suffix
    // denotes "reverse-holo style overlay treatment".
    "pokeball_rh",
    "masterball_rh",
    "quickball_rh",
    "duskball_rh",
    "loveball_rh",
    "friendball_rh",
    "energy_symbol_rh",
    "team_rocket_rh",
    "cosmos_holo",
    "double_rare",
    "ultra_rare",
    "illustration_rare",
    "special_illustration_rare",
    "hyper_rare",
    "rainbow_rare",
    "shiny_rare",
    "ace_spec",
    "mega_attack_rare",
    "mega_hyper_rare",
    "galarian_gallery",
    "trainer_gallery",
    "promo_blackstar",
    "stamp_prerelease",
    "stamp_buildbattle",
    "stamp_pokemoncenter",
    "stamp_staff",
];

/// Extract the pattern/treatment variant carried by a TCGplayer product
/// name. Returns `None` for the card's base product (covers normal /
/// reverse_holo / holo via its own sub_types); returns `Some(variant)` for
/// separately-keyed pattern or foil-treatment products (Master Ball, Cosmos
/// Holo, Water Web Holo, etc.).
///
/// EVERY parenthetical is consulted, left to right, and the first that names
/// a treatment wins. MCAP products in particular stack a foil treatment with
/// a retailer tag — "(Reverse Cosmos Holo) (Costco Exclusive)" — and we want
/// the physical foil, not the store. A card whose own name contains a
/// pattern token ("Team Rocket's Spidops") is unaffected: `treatment_for`
/// only fires on the exact treatment phrases, which a Pokémon name won't
/// carry inside parens. Match order within a paren is significant — more
/// specific tokens first so "Reverse Cosmos Holo" doesn't fall through to
/// the "Cosmos Holo" rule, and "Master Ball" doesn't hit a bare "Ball" rule.
pub fn variant_from_product_name(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    let mut rest = lower.as_str();
    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else { break };
        let inner = after[..close].trim();
        if let Some(v) = treatment_for(inner) {
            return Some(v);
        }
        rest = &after[close + 1..];
    }
    None
}

/// Map the trimmed, lowercased text inside one parenthetical to a treatment
/// variant code. Returns `None` when the text isn't a recognized treatment
/// (a retailer tag, an event name, a card-name forme, etc.). Ordering is
/// specific-before-general.
fn treatment_for(inner: &str) -> Option<&'static str> {
    // Ball / symbol reverse-holo patterns.
    if inner.contains("master ball") {
        Some("masterball_rh")
    } else if inner.contains("quick ball") {
        Some("quickball_rh")
    } else if inner.contains("dusk ball") {
        Some("duskball_rh")
    } else if inner.contains("love ball") {
        Some("loveball_rh")
    } else if inner.contains("friend ball") {
        Some("friendball_rh")
    } else if inner.contains("poke ball") || inner.contains("poké ball") {
        Some("pokeball_rh")
    } else if inner.contains("energy symbol") {
        Some("energy_symbol_rh")
    } else if inner.contains("team rocket") {
        Some("team_rocket_rh")
    // Cosmos family — specific variants before the bare "cosmos holo".
    } else if inner.contains("reverse cosmos") {
        Some("reverse_cosmos_holo")
    } else if inner.contains("pixel") {
        // "Pixel Holo" / "Pixel Cosmos Holo" — same pixelated treatment.
        Some("pixel_holo")
    } else if inner.contains("cosmo holo") || inner.contains("cosmos holo") {
        // MCAP (group 2374) hosts cosmos-holo reprints of numbered-set
        // cards, e.g. "Erika's Tangela - 007/217 (Cosmo Holo)" — the
        // cross-group matcher in pkdump-ingest attaches them to the
        // base card as a fourth variant.
        Some("cosmos_holo")
    } else if inner.contains("cosmo foil") || inner.contains("cosmos foil") {
        Some("cosmos_foil")
    // Other named foil treatments hosted in MCAP.
    } else if inner.contains("water web") {
        Some("water_web_holo")
    } else if inner.contains("sheen holo") {
        Some("sheen_holo")
    } else if inner.contains("mirage holo") {
        Some("mirage_holo")
    } else if inner.contains("mirror holo") {
        // The Japanese reverse-holo treatment. TCGCSV tags it
        // "(Mirror Holofoil)" on a product that shares the base card's
        // collector number, so without this the two would collide on
        // the same sub_type and one printing would be lost.
        Some("mirror_holo")
    } else if inner.contains("line holo") {
        Some("line_holo")
    } else if inner.contains("sparkle holo") {
        Some("sparkle_holo")
    } else if inner.contains("energy holo") {
        Some("energy_holo")
    } else if inner.contains("metal card") {
        // "Metal Card" / "Celebrations Metal Card" / "GameStop Metal Card".
        Some("metal_card")
    } else if inner.contains("cracked ice") {
        // Etched ice-pattern foil ("Cracked Ice" / "Cracked Ice Holo").
        Some("cracked_ice_holo")
    } else if inner.contains("peelable ditto") {
        // Pokemon GO has three cards (Bidoof, Numel, Spinarak) with
        // peelable Ditto variants that share a collector number with
        // their non-peelable counterparts.
        Some("peelable_ditto")
    } else if inner.contains("black dot error") {
        // Base Set Charizard #4 has a (Black Dot Error) misprint
        // variant sharing the same collector number as the corrected
        // print.
        Some("black_dot_error")
    } else {
        None
    }
}

/// Parse a stamped-promo TCGplayer product name into a (variant_code,
/// optional set keyword). Stamps live in a different TCGCSV group than
/// their base set (the "Miscellaneous Cards and Products" catch-all), so
/// the product-name suffix is the bridge.
///
/// Set keyword, when present, narrows the base-card match to a set whose
/// name contains it — load-bearing for cases where multiple sets share a
/// printed_total (e.g. BLK and WHT both 86 cards). When `None`, the
/// caller falls back to matching the parsed card name + collector number
/// + printed_total against the catalog.
///
/// Examples:
///   "Victini (Black Bolt Stamped)"
///     -> Some(("stamp_black_bolt", Some("black bolt")))
///   "Pikachu - 58/102 (E3 Stamped)"
///     -> Some(("stamp_e3", Some("e3")))
///   "Ditto - 39/113 (SDCC Stamp)"
///     -> Some(("stamp_sdcc", Some("sdcc")))
///   "Buck's Training - 130/146 (Prerelease)"
///     -> Some(("stamp_prerelease", None))
///   "Buck's Training - 130/146 (Prerelease) [Staff]"
///     -> Some(("stamp_prerelease_staff", None))
///   "Shellos West Sea (SDCC 2007 Staff)"
///     -> Some(("stamp_sdcc_2007_staff", None))
///   "Pikachu (Toys R Us Promo)"
///     -> None  // generic promo isn't a stamp
pub fn parse_stamp_tag(name: &str) -> Option<(String, Option<String>)> {
    // Lowercase + lightweight accent fold. TCGCSV spells "Pokémon" both
    // ways across the catalog — same variant either way.
    let lower = name.to_lowercase().replace(['é', 'è', 'ê'], "e");
    let bracket_staff = lower.contains("[staff]");
    let bracket_winner = lower.contains("[winner]");

    // Pull the LAST parenthetical (some products carry both "(...)" and a
    // trailing "[Staff]"/"[Winner]" bracket).
    let inner = lower.rfind('(').and_then(|open| {
        lower[open..]
            .find(')')
            .map(|close| lower[open + 1..open + close].trim().to_string())
    });

    let (mut variant, keyword) = if let Some(inner) = inner {
        if let Some(stripped) = strip_stamp_suffix(&inner) {
            // "(X Stamped)" / "(X Stamp)" — keyword = X (used to
            // disambiguate among sets sharing a printed_total).
            let keyword = stripped.trim().to_string();
            if keyword.is_empty() {
                return None;
            }
            (format!("stamp_{}", to_snake(&keyword)), Some(keyword))
        } else if inner == "prerelease" {
            ("stamp_prerelease".to_string(), None)
        } else if inner == "pokemon center" || inner == "pokemon center exclusive" {
            // PC-stamped reprints of regular promos. TCGCSV uses both
            // phrasings interchangeably — both fold into the same
            // `stamp_pokemoncenter` variant (seeded in V2) so the
            // booster/ETB version and the PC version surface as two
            // distinct printings on the same card.
            ("stamp_pokemoncenter".to_string(), None)
        } else if inner == "holiday calendar" {
            // Prismatic Evolutions advent product — gives 26 cards an
            // alternate-treatment variant that shares its collector
            // number with the regular ETB/booster print.
            ("stamp_holiday_calendar".to_string(), None)
        } else if inner == "staff" {
            // Bare "(Staff)" — older convention than "[Staff]" but
            // TCGCSV uses both interchangeably for staff promos.
            ("stamp_staff".to_string(), None)
        } else if inner.contains("championship") {
            // (World Championship 2025), (Asia Championship Series 23-24)
            // and similar event stamps. Generic so we don't have to
            // enumerate every year × region the catalog will see.
            (format!("stamp_{}", to_snake(&inner)), None)
        } else if inner.ends_with(" staff") {
            // "(SDCC 2007 Staff)" — paren-internal Staff, no separate
            // [Staff] bracket. Treat the part before "staff" as the
            // event identifier; the resulting variant naturally ends in
            // "_staff", so we won't append it twice below.
            let event = inner[..inner.len() - " staff".len()].trim();
            if event.is_empty() {
                return None;
            }
            (format!("stamp_{}_staff", to_snake(event)), None)
        } else if bracket_staff || bracket_winner {
            // Some other paren content (an event marker like "SDCC 2009",
            // a venue like "Pokemon Center Exclusive") combined with a
            // [Staff]/[Winner] bracket — treat the paren as the event
            // identifier and let the suffix-appender below add the role.
            (format!("stamp_{}", to_snake(&inner)), None)
        } else {
            return None;
        }
    } else if bracket_staff {
        ("stamp_staff".to_string(), None)
    } else if bracket_winner {
        ("stamp_winner".to_string(), None)
    } else {
        return None;
    };

    // Append the bracket role if present and not already encoded.
    if bracket_staff && !variant.ends_with("_staff") {
        variant.push_str("_staff");
    }
    if bracket_winner && !variant.ends_with("_winner") {
        variant.push_str("_winner");
    }
    Some((variant, keyword))
}

/// Strip a trailing " stamp" or " stamped" word from a stamped-paren's
/// inner token. Returns the remainder (which becomes the keyword), or
/// `None` if the inner doesn't end with either form.
fn strip_stamp_suffix(inner: &str) -> Option<&str> {
    for suffix in [" stamped", " stamp"] {
        if let Some(rest) = inner.strip_suffix(suffix) {
            return Some(rest);
        }
        // "X Stamped with Y" — keep everything before "stamped" as the
        // keyword, dropping any post-suffix qualifier.
        if let Some((before, _)) = inner.split_once(suffix.trim_start())
            && !before.is_empty()
            && before.ends_with(' ')
        {
            return Some(before.trim_end());
        }
    }
    None
}

/// Reduce a free-form label to snake_case, ASCII alphanumeric only.
fn to_snake(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

/// Parse the card name from a TCGplayer product name. Returns the
/// substring before the first ` - ` (the collector-number separator) or
/// the first ` (` (the parenthetical-tag opener), whichever comes first.
/// Used by the stamp matcher when no set keyword is available, to filter
/// candidate cards by name.
pub fn parse_product_card_name(product_name: &str) -> &str {
    let dash = product_name.find(" - ").unwrap_or(product_name.len());
    let paren = product_name.find(" (").unwrap_or(product_name.len());
    let cut = dash.min(paren);
    let name = product_name[..cut].trim();
    // Some MCAP products separate the collector number with a bare space
    // instead of " - " — "Gengar 094/165 (Cosmos Holo)". Strip that trailing
    // number token so the base name matches the catalog card.
    if let Some((head, last)) = name.rsplit_once(' ')
        && is_collector_number_token(last)
    {
        return head.trim();
    }
    name
}

/// Whether a whitespace-separated token looks like a trailing collector
/// number (e.g. "094/165", "14/181"). Conservative: must contain a digit
/// and either a slash or be all digits — so name words like "ex", "V", or
/// "GX" are never stripped.
fn is_collector_number_token(tok: &str) -> bool {
    let has_digit = tok.bytes().any(|b| b.is_ascii_digit());
    let slash_or_all_digits = tok.contains('/') || tok.bytes().all(|b| b.is_ascii_digit());
    has_digit && slash_or_all_digits && !tok.is_empty()
}

/// Predicate part of an overlay rule. A `None` field matches anything; every
/// field that *is* set must match.
#[derive(Debug, Clone, Deserialize)]
pub struct OverrideMatch {
    pub set: Option<String>,
    pub rarity: Option<Vec<String>>,
    pub number: Option<String>,
}

impl OverrideMatch {
    /// Whether this predicate matches a given card.
    pub fn matches(&self, set_code: &str, rarity: Option<&str>, number: &str) -> bool {
        if let Some(s) = &self.set
            && s != set_code
        {
            return false;
        }
        if let Some(n) = &self.number
            && n != number
        {
            return false;
        }
        if let Some(rarities) = &self.rarity {
            match rarity {
                Some(r) if rarities.iter().any(|x| x == r) => {}
                _ => return false,
            }
        }
        true
    }
}

/// One record from `data/overrides/variant_augmentations.json`. Applies on
/// top of TCGCSV-derived variants — `add` injects variants TCGCSV hasn't
/// (or can't) modeled, `remove` strips ones the upstream data over-reports.
#[derive(Debug, Clone, Deserialize)]
pub struct VariantOverride {
    #[serde(rename = "match")]
    pub match_: OverrideMatch,
    #[serde(default)]
    pub add: Vec<String>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub comment: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variant_from_product_name_only_consults_trailing_paren() {
        // Cards whose own name contains a pattern token must not be
        // misidentified as a pattern product when the trailing paren
        // (or absence of one) doesn't actually carry the marker.
        assert_eq!(variant_from_product_name("Team Rocket's Spidops"), None);
        assert_eq!(
            variant_from_product_name("Team Rocket's Mimikyu - 087/182"),
            None
        );
        // The same card with an explicit pattern paren still resolves.
        assert_eq!(
            variant_from_product_name("Team Rocket's Spidops (Team Rocket)"),
            Some("team_rocket_rh")
        );
        assert_eq!(
            variant_from_product_name("Team Rocket's Spidops (Energy Symbol Pattern)"),
            Some("energy_symbol_rh")
        );
    }

    #[test]
    fn variant_from_product_name_picks_foil_treatments_and_scans_all_parens() {
        // Named foil treatments hosted in MCAP.
        assert_eq!(
            variant_from_product_name("Charcadet - 026/182 (Cosmos Foil)"),
            Some("cosmos_foil")
        );
        assert_eq!(
            variant_from_product_name("Alolan Ninetales - 28/147 (Water Web Holo)"),
            Some("water_web_holo")
        );
        assert_eq!(
            variant_from_product_name("Blacksmith - 88/106 (Sheen Holo)"),
            Some("sheen_holo")
        );
        // Metal-card collectibles fold together regardless of the prefix.
        assert_eq!(
            variant_from_product_name("Pikachu (Celebrations Metal Card)"),
            Some("metal_card")
        );
        // Double parenthetical: the FOIL wins over the retailer tag, and the
        // foil is the FIRST paren (so all parens must be scanned, not just
        // the trailing one).
        assert_eq!(
            variant_from_product_name(
                "Bulbasaur - 001/165 (Reverse Cosmos Holo) (Costco Exclusive)"
            ),
            Some("reverse_cosmos_holo")
        );
        assert_eq!(
            variant_from_product_name("Archaludon (Cosmos Holo) (Gamestop Exclusive)"),
            Some("cosmos_holo")
        );
        // "Reverse Cosmos" must not fall through to plain "cosmos holo".
        assert_eq!(
            variant_from_product_name("Foo (Reverse Cosmos Holo)"),
            Some("reverse_cosmos_holo")
        );
        // Pixel Cosmos Holo folds into pixel_holo, ahead of the cosmos rule.
        assert_eq!(
            variant_from_product_name("Rayquaza - SWSH029 (Pixel Cosmos Holo)"),
            Some("pixel_holo")
        );
        // A pure retailer/event tag is NOT a foil treatment → None here
        // (the MCAP `promo` fallback in pkdump-ingest handles those).
        assert_eq!(
            variant_from_product_name("Bulbasaur - 1/165 (Best Buy Exclusive)"),
            None
        );
        assert_eq!(variant_from_product_name("Ancient Mew"), None);
    }

    #[test]
    fn variant_from_product_name_picks_pattern_codes() {
        assert_eq!(variant_from_product_name("Bulbasaur - 001/165"), None);
        assert_eq!(
            variant_from_product_name("Bulbasaur (Poke Ball Pattern)"),
            Some("pokeball_rh")
        );
        assert_eq!(
            variant_from_product_name("Bulbasaur (Poké Ball Pattern)"),
            Some("pokeball_rh")
        );
        assert_eq!(
            variant_from_product_name("Swadloon (Master Ball Pattern)"),
            Some("masterball_rh")
        );
        assert_eq!(
            variant_from_product_name("Dreepy - 158/217 (Energy Symbol Pattern)"),
            Some("energy_symbol_rh")
        );
        assert_eq!(
            variant_from_product_name("Dreepy - 158/217 (Quick Ball)"),
            Some("quickball_rh")
        );
        assert_eq!(
            variant_from_product_name("Klink (Dusk Ball)"),
            Some("duskball_rh")
        );
        assert_eq!(
            variant_from_product_name("Klink (Love Ball)"),
            Some("loveball_rh")
        );
        assert_eq!(
            variant_from_product_name("Klink (Friend Ball)"),
            Some("friendball_rh")
        );
        // "Team Rocket's <X>" is a card *name*, not a Team Rocket pattern
        // product — the actual pattern variant lives in the trailing
        // paren and is recognised correctly.
        assert_eq!(variant_from_product_name("Team Rocket's Tarountula"), None);
        assert_eq!(
            variant_from_product_name("Team Rocket's Tarountula (Team Rocket)"),
            Some("team_rocket_rh")
        );
        // MCAP-style cosmos-holo reprints — both "Cosmo Holo" (the
        // current ASC era) and the older "Cosmos Holo" / "Cosmos
        // Holofoil" spellings.
        assert_eq!(
            variant_from_product_name("Erika's Tangela - 007/217 (Cosmo Holo)"),
            Some("cosmos_holo")
        );
        assert_eq!(
            variant_from_product_name("Rowlet - 9/149 (Cosmos Holo)"),
            Some("cosmos_holo")
        );
        assert_eq!(
            variant_from_product_name("Cynthia's Feelings - 131/146 (Cosmos Holofoil)"),
            Some("cosmos_holo")
        );

        // Cracked Ice Holo — a distinct foiling treatment that
        // otherwise collides with the base holo on the same number
        // (e.g. every SVE basic energy has both a regular Holofoil and
        // a Cracked Ice Holo product).
        assert_eq!(
            variant_from_product_name("Basic Grass Energy (Cracked Ice Holo)"),
            Some("cracked_ice_holo")
        );

        // Peelable Ditto — Pokemon GO booster transform cards (Bidoof,
        // Spinarak, Numel each have a peelable variant).
        assert_eq!(
            variant_from_product_name("Spinarak (Peelable Ditto)"),
            Some("peelable_ditto")
        );

        // Charizard Base Set 4 has a one-off (Black Dot Error) variant.
        assert_eq!(
            variant_from_product_name("Charizard (Black Dot Error)"),
            Some("black_dot_error")
        );
    }

    #[test]
    fn parse_stamp_tag_extracts_variant_and_keyword() {
        // Set-named "(X Stamped)" — keyword present, used to
        // disambiguate among sets sharing a printed_total.
        assert_eq!(
            parse_stamp_tag("Victini (Black Bolt Stamped)"),
            Some(("stamp_black_bolt".into(), Some("black bolt".into())))
        );
        assert_eq!(
            parse_stamp_tag("Corviknight - 156/189 (Darkness Ablaze Stamped)"),
            Some((
                "stamp_darkness_ablaze".into(),
                Some("darkness ablaze".into())
            ))
        );
        assert_eq!(
            parse_stamp_tag("Pikachu - 58/102 (E3 Stamped)"),
            Some(("stamp_e3".into(), Some("e3".into())))
        );
        assert_eq!(
            parse_stamp_tag("Ditto - 39/113 (SDCC Stamp)"),
            Some(("stamp_sdcc".into(), Some("sdcc".into())))
        );

        // Prerelease — no set keyword, caller matches by card name + total.
        assert_eq!(
            parse_stamp_tag("Buck's Training - 130/146 (Prerelease)"),
            Some(("stamp_prerelease".into(), None))
        );
        // Prerelease + [Staff] = composite variant code.
        assert_eq!(
            parse_stamp_tag("Buck's Training - 130/146 (Prerelease) [Staff]"),
            Some(("stamp_prerelease_staff".into(), None))
        );
        // (Event) alone — no stamp/staff marker — returns None.
        assert_eq!(parse_stamp_tag("Riolu - 91/127 (SDCC 2009)"), None);
        // (Event) + [Staff] — variant code merges the event into the
        // staff suffix.
        assert_eq!(
            parse_stamp_tag("Riolu - 91/127 (SDCC 2009) [Staff]"),
            Some(("stamp_sdcc_2009_staff".into(), None))
        );
        // Paren-internal Staff with no [Staff] bracket.
        assert_eq!(
            parse_stamp_tag("Shellos West Sea (SDCC 2007 Staff)"),
            Some(("stamp_sdcc_2007_staff".into(), None))
        );

        // Pokemon Center variants — both phrasings TCGCSV uses fold
        // into the seeded `stamp_pokemoncenter` code so the regular
        // and PC-stamped products of the same card surface as two
        // distinct printings (they're priced very differently and
        // collectors track them separately). The é/e accent variant
        // also folds in.
        assert_eq!(
            parse_stamp_tag("Charcadet - 022 (Pokemon Center Exclusive)"),
            Some(("stamp_pokemoncenter".into(), None))
        );
        assert_eq!(
            parse_stamp_tag("Flutter Mane - 097 (Pokemon Center)"),
            Some(("stamp_pokemoncenter".into(), None))
        );
        assert_eq!(
            parse_stamp_tag("Magneton - 159 (Pokémon Center Exclusive)"),
            Some(("stamp_pokemoncenter".into(), None))
        );

        // Bare (Staff) — TCGCSV uses this where the older convention
        // was `[Staff]`. Both fold to stamp_staff.
        assert_eq!(
            parse_stamp_tag("Gouging Fire - 151 (Staff)"),
            Some(("stamp_staff".into(), None))
        );

        // (Holiday Calendar) — the Prismatic Evolutions advent product.
        assert_eq!(
            parse_stamp_tag("Glaceon ex - 026/131 (Holiday Calendar)"),
            Some(("stamp_holiday_calendar".into(), None))
        );

        // Championship events — generic "contains 'championship'" rule
        // so we don't have to enumerate every year × series TCGCSV
        // catalogs.
        assert_eq!(
            parse_stamp_tag("Pikachu - 225 (World Championship 2025)"),
            Some(("stamp_world_championship_2025".into(), None))
        );
        assert_eq!(
            parse_stamp_tag("Pikachu - 101 (Asia Championship Series 23-24)"),
            Some(("stamp_asia_championship_series_23_24".into(), None))
        );

        // [Winner] bracket — analogous to [Staff]. Pikachu 225 has both
        // a base (World Championship 2025) product and a (...) [Winner]
        // variant that gets its own stamp_*_winner code.
        assert_eq!(
            parse_stamp_tag("Pikachu - 225 (World Championship 2025) [Winner]"),
            Some(("stamp_world_championship_2025_winner".into(), None))
        );

        // Non-stamps return None.
        assert_eq!(parse_stamp_tag("Pikachu (Toys R Us Promo)"), None);
        assert_eq!(parse_stamp_tag("Bulbasaur - 001/165"), None);
        assert_eq!(parse_stamp_tag("Bulbasaur (Master Ball Pattern)"), None);
    }

    #[test]
    fn parse_product_card_name_strips_dash_or_paren_suffix() {
        assert_eq!(
            parse_product_card_name("Buck's Training - 130/146 (Prerelease)"),
            "Buck's Training"
        );
        assert_eq!(
            parse_product_card_name("Victini (Black Bolt Stamped)"),
            "Victini"
        );
        assert_eq!(
            parse_product_card_name("Team Rocket's Mimikyu (Prerelease) [Staff]"),
            "Team Rocket's Mimikyu"
        );
        assert_eq!(parse_product_card_name("Bulbasaur"), "Bulbasaur");
        // Space-separated collector number (no " - "): strip the number.
        assert_eq!(
            parse_product_card_name("Gengar 094/165 (Cosmos Holo)"),
            "Gengar"
        );
        assert_eq!(
            parse_product_card_name("Dragonite 149/165 (Cosmos Holo)"),
            "Dragonite"
        );
        // A name word with a digit that isn't a collector number is kept
        // (no slash, not all-digits) — e.g. "Porygon2" stays intact.
        assert_eq!(parse_product_card_name("Cool Porygon2"), "Cool Porygon2");
    }

    #[test]
    fn override_match_predicates() {
        let json = r#"{"set":"sv3pt5","rarity":["Common"]}"#;
        let m: OverrideMatch = serde_json::from_str(json).unwrap();
        assert!(m.matches("sv3pt5", Some("Common"), "4"));
        assert!(!m.matches("sv4", Some("Common"), "4"));
        assert!(!m.matches("sv3pt5", Some("Rare"), "4"));

        let by_number: OverrideMatch =
            serde_json::from_str(r#"{"set":"sv3pt5","number":"4"}"#).unwrap();
        assert!(by_number.matches("sv3pt5", Some("Common"), "4"));
        assert!(!by_number.matches("sv3pt5", Some("Common"), "5"));
    }
}
