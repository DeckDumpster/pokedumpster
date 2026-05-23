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
/// Match order is significant: more-specific tokens first so e.g.
/// "Master Ball" doesn't fall through to a "Ball" rule.
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
            variant_from_product_name("Dreepy - 158/217 - Energy Symbol Pattern"),
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
        assert_eq!(
            variant_from_product_name("Team Rocket's Tarountula"),
            Some("team_rocket_rh")
        );
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
