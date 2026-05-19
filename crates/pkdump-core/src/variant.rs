//! Three-layer variant expansion (PLAN.md §4).
//!
//! No upstream source knows the full set of printings for a card. PokeDumpster
//! layers three:
//!
//! 1. **data-driven** — variants implied by the card's TCGplayer price keys;
//! 2. **rarity bootstrap** — used only when layer 1 is empty (a brand-new set
//!    pokemontcg.io has not priced yet);
//! 3. **JSON overlay** — hand-curated `add`/`remove` rules from
//!    `data/overrides/variant_augmentations.json`, applied last so they
//!    always win.

use std::collections::BTreeSet;

use serde::Deserialize;

/// pokemontcg.io TCGplayer price key → PokeDumpster variant code (layer 1).
const PRICE_KEY_TO_VARIANT: &[(&str, &str)] = &[
    ("normal", "normal"),
    ("holofoil", "holo"),
    ("reverseHolofoil", "reverse_holo"),
    ("1stEditionHolofoil", "first_ed_holo"),
    ("1stEditionNormal", "first_ed_normal"),
    ("unlimitedHolofoil", "unlimited_holo"),
];

/// Variant codes PokeDumpster recognises (RESEARCH.md §4.2). Reference list;
/// the overlay may legitimately introduce others as new gimmicks ship.
pub const KNOWN_VARIANTS: &[&str] = &[
    "normal",
    "holo",
    "reverse_holo",
    "first_ed_holo",
    "first_ed_normal",
    "unlimited_holo",
    "pokeball_rh",
    "masterball_rh",
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

/// Layer 1 — variants implied by the card's TCGplayer price keys.
pub fn variants_from_price_keys(price_keys: &[String]) -> Vec<String> {
    price_keys
        .iter()
        .filter_map(|key| {
            PRICE_KEY_TO_VARIANT
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        })
        .collect()
}

/// Layer 2 — used only when layer 1 yields nothing. Commons/Uncommons/Rares
/// get a regular plus a reverse holo; everything else (holo rares and the
/// special rarities) gets a single holo printing.
pub fn bootstrap_from_rarity(rarity: Option<&str>) -> Vec<String> {
    match rarity {
        Some("Common" | "Uncommon" | "Rare") => {
            vec!["normal".into(), "reverse_holo".into()]
        }
        Some(r) if !r.is_empty() => vec!["holo".into()],
        _ => vec!["normal".into()],
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

/// One record from `data/overrides/variant_augmentations.json` (layer 3).
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

/// Run the full three-layer expansion for one card. The returned variant
/// codes are sorted and de-duplicated.
pub fn expand_variants(
    set_code: &str,
    number: &str,
    rarity: Option<&str>,
    price_keys: &[String],
    overrides: &[VariantOverride],
) -> Vec<String> {
    let mut variants: BTreeSet<String> = variants_from_price_keys(price_keys).into_iter().collect();
    if variants.is_empty() {
        variants.extend(bootstrap_from_rarity(rarity));
    }
    for ov in overrides {
        if ov.match_.matches(set_code, rarity, number) {
            for v in &ov.add {
                variants.insert(v.clone());
            }
            for v in &ov.remove {
                variants.remove(v);
            }
        }
    }
    variants.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn layer1_maps_price_keys() {
        let v = variants_from_price_keys(&s(&["normal", "reverseHolofoil"]));
        assert_eq!(v, s(&["normal", "reverse_holo"]));
        // unknown keys are ignored
        assert!(variants_from_price_keys(&s(&["mysteryFoil"])).is_empty());
    }

    #[test]
    fn layer2_bootstrap_by_rarity() {
        assert_eq!(
            bootstrap_from_rarity(Some("Common")),
            s(&["normal", "reverse_holo"])
        );
        assert_eq!(
            bootstrap_from_rarity(Some("Special Illustration Rare")),
            s(&["holo"])
        );
        assert_eq!(bootstrap_from_rarity(None), s(&["normal"]));
    }

    #[test]
    fn expand_prefers_price_keys_over_bootstrap() {
        let v = expand_variants("sv3pt5", "4", Some("Common"), &s(&["holofoil"]), &[]);
        assert_eq!(v, s(&["holo"])); // not the rarity bootstrap
    }

    #[test]
    fn expand_bootstraps_when_no_price_keys() {
        let v = expand_variants("sv99", "1", Some("Common"), &[], &[]);
        assert_eq!(v, s(&["normal", "reverse_holo"]));
    }

    #[test]
    fn overlay_add_and_remove_apply_last() {
        let json = r#"[
            {"match":{"set":"sv3pt5","rarity":["Common"]},"add":["pokeball_rh","masterball_rh"]},
            {"match":{"set":"sv3pt5","number":"4"},"remove":["reverse_holo"]}
        ]"#;
        let overrides: Vec<VariantOverride> = serde_json::from_str(json).unwrap();
        let v = expand_variants(
            "sv3pt5",
            "4",
            Some("Common"),
            &s(&["normal", "reverseHolofoil"]),
            &overrides,
        );
        // reverse_holo removed, the two ball patterns added
        assert_eq!(v, s(&["masterball_rh", "normal", "pokeball_rh"]));
    }

    #[test]
    fn overlay_match_is_scoped() {
        let json = r#"[{"match":{"set":"sv3pt5","rarity":["Common"]},"add":["pokeball_rh"]}]"#;
        let overrides: Vec<VariantOverride> = serde_json::from_str(json).unwrap();
        // wrong set — rule does not fire
        let v = expand_variants("sv4", "4", Some("Common"), &s(&["normal"]), &overrides);
        assert_eq!(v, s(&["normal"]));
        // wrong rarity — rule does not fire
        let v = expand_variants("sv3pt5", "4", Some("Rare"), &s(&["normal"]), &overrides);
        assert_eq!(v, s(&["normal"]));
    }
}
