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
    // distinguish themselves via the sub_type. `sub_type_to_variant`
    // owns the mapping.
    "normal",
    "holo",
    "reverse_holo",
    "first_ed_holo",
    "first_ed_normal",
    "unlimited_holo",
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

/// Map a TCGCSV `sub_type_name` (as it appears in the `prices` table) to a
/// PokeDumpster base-variant code. Returns `None` for sub_types we don't
/// model — those products typically belong to non-base treatments handled
/// via `variant_from_product_name`.
pub fn sub_type_to_variant(sub_type: &str) -> Option<&'static str> {
    match sub_type {
        "Normal" => Some("normal"),
        "Holofoil" => Some("holo"),
        "Reverse Holofoil" => Some("reverse_holo"),
        "1st Edition Holofoil" => Some("first_ed_holo"),
        "1st Edition Normal" => Some("first_ed_normal"),
        "Unlimited Holofoil" => Some("unlimited_holo"),
        _ => None,
    }
}

/// Extract the pattern variant carried by a TCGplayer product name. Returns
/// `None` for the card's base product (covers normal / reverse_holo / holo
/// via its own sub_types); returns `Some(variant)` for separately-keyed
/// pattern products (Master Ball, Energy Symbol, etc.).
///
/// Only the trailing parenthetical is consulted — a card whose own name
/// contains a pattern token ("Team Rocket's Spidops", "Pokémon Center
/// Tin") would otherwise be misidentified as a pattern product. Match
/// order is significant: more-specific tokens first so e.g.
/// "Master Ball" doesn't fall through to a "Ball" rule.
pub fn variant_from_product_name(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    let open = lower.rfind('(')?;
    let close = lower[open..].find(')').map(|i| open + i)?;
    let inner = lower[open + 1..close].trim();
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
    let lower = name.to_lowercase();
    let bracket_staff = lower.contains("[staff]");

    // Pull the LAST parenthetical (some products carry both "(...)" and a
    // trailing "[Staff]" bracket).
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
        } else if bracket_staff {
            // Some other paren content (an event marker like "SDCC 2009",
            // a venue like "Pokemon Center Exclusive") combined with a
            // [Staff] bracket — treat the paren as the event identifier
            // and let the suffix-appender below add "_staff".
            (format!("stamp_{}", to_snake(&inner)), None)
        } else {
            return None;
        }
    } else if bracket_staff {
        ("stamp_staff".to_string(), None)
    } else {
        return None;
    };

    // Append "_staff" if the [Staff] bracket was present and we haven't
    // already encoded it.
    if bracket_staff && !variant.ends_with("_staff") {
        variant.push_str("_staff");
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
    product_name[..cut].trim()
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
    fn sub_type_to_variant_covers_the_six_base_treatments() {
        assert_eq!(sub_type_to_variant("Normal"), Some("normal"));
        assert_eq!(sub_type_to_variant("Holofoil"), Some("holo"));
        assert_eq!(
            sub_type_to_variant("Reverse Holofoil"),
            Some("reverse_holo")
        );
        assert_eq!(
            sub_type_to_variant("1st Edition Holofoil"),
            Some("first_ed_holo")
        );
        assert_eq!(
            sub_type_to_variant("1st Edition Normal"),
            Some("first_ed_normal")
        );
        assert_eq!(
            sub_type_to_variant("Unlimited Holofoil"),
            Some("unlimited_holo")
        );
        assert_eq!(sub_type_to_variant("Anything Else"), None);
    }

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
