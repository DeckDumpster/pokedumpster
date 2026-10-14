//! What is PokeDumpster's, and only that, over the lake that `dumpster-kit` owns.
//!
//! The kit's lake, keys, shipper and eraser are one implementation each and they
//! enumerate no site's upstreams. A site states three things: the prefix its
//! configuration names derive from, the upstreams it lands, and the datasets it
//! lands from each. This crate is those three and nothing else; there is no
//! behaviour in it to diverge from the kit's.
//!
//! The strings below are on-disk layout: `raw/source=<source>/dataset=<dataset>/…`.
//! A change to one orphans every object already landed, which is why they are
//! spelled once, here, and held by [`tests::the_landed_layout_is_unchanged`].

use dumpster_kit_keys::master::KeyEnv;
use dumpster_kit_lake::{Dataset, LakeEnv, Source};
use dumpster_kit_tenancy::AppPrefix;

/// `PKDUMP` — every `PKDUMP_*` variable and `~/.config/pkdump/*` path derives from it.
pub const PREFIX: &str = "PKDUMP";

/// This site's identity.
pub fn app() -> AppPrefix {
    AppPrefix::new(PREFIX).expect("PKDUMP is a valid app prefix")
}

/// The lake's configuration names (`PKDUMP_LAKE_S3_BUCKET`, `~/.config/pkdump/lake.env`, …).
pub fn lake_env() -> LakeEnv {
    LakeEnv::new(&app())
}

/// Key custody's names (`PKDUMP_MASTER_KEY_FILE`, `~/.config/pkdump/tenant-master.key`) and the
/// root of every HKDF label.
pub fn key_env() -> KeyEnv {
    KeyEnv::new(&app())
}

/// `tcgcsv.com/tcgplayer` — groups, products, prices.
pub const TCGCSV: Source = Source::TCGCSV;
/// `api.pokemontcg.io/v2` — sets, cards.
pub const POKEMON_TCG_IO: Source = Source::new("pokemontcgio");
/// `codeload.github.com` — the pokemon-tcg-data bulk corpus.
pub const POKEMON_TCG_DATA: Source = Source::new("pokemon-tcg-data");

/// TCGCSV `/groups`.
pub const GROUPS: Dataset = Dataset::GROUPS;
/// TCGCSV `/{group}/products`.
pub const PRODUCTS: Dataset = Dataset::PRODUCTS;
/// TCGCSV `/{group}/prices`.
pub const PRICES: Dataset = Dataset::PRICES;
/// pokemontcg.io `/v2/sets`.
pub const SETS: Dataset = Dataset::new("sets");
/// pokemontcg.io `/v2/cards`.
pub const CARDS: Dataset = Dataset::new("cards");
/// The pokemon-tcg-data bulk tarball.
pub const BULK: Dataset = Dataset::new("bulk");

/// Every `(source, dataset)` this site lands, in the order a derive chooses and reports them.
///
/// `images.pokemontcg.io` is deliberately absent and always will be: the retention arithmetic
/// that justifies keeping `raw/` forever is for JSON only.
pub const DATASETS: &[(Source, Dataset)] = &[
    (TCGCSV, GROUPS),
    (TCGCSV, PRODUCTS),
    (TCGCSV, PRICES),
    (POKEMON_TCG_IO, SETS),
    (POKEMON_TCG_IO, CARDS),
    (POKEMON_TCG_DATA, BULK),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The strings are the layout of every object already in the bucket.
    #[test]
    fn the_landed_layout_is_unchanged() {
        let got: Vec<(&str, &str)> = DATASETS
            .iter()
            .map(|(s, d)| (s.as_str(), d.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("tcgcsv", "groups"),
                ("tcgcsv", "products"),
                ("tcgcsv", "prices"),
                ("pokemontcgio", "sets"),
                ("pokemontcgio", "cards"),
                ("pokemon-tcg-data", "bulk"),
            ]
        );
    }

    /// Every name the operator's runbooks, units and `lake.env` spell is derived from the one
    /// prefix; this is what a rename of `PKDUMP` would have to move.
    #[test]
    fn the_names_are_the_ones_the_runbooks_spell() {
        let lake = lake_env();
        assert_eq!(lake.bucket, "PKDUMP_LAKE_S3_BUCKET");
        assert_eq!(lake.region, "PKDUMP_LAKE_S3_REGION");
        assert_eq!(lake.prefix, "PKDUMP_LAKE_S3_PREFIX");
        assert_eq!(lake.endpoint, "PKDUMP_LAKE_S3_ENDPOINT");
        assert_eq!(lake.dir, "PKDUMP_LAKE_DIR");
        assert_eq!(lake.env_file, "PKDUMP_LAKE_ENV");
        assert_eq!(lake.tenant_profile, "PKDUMP_TENANT_AWS_PROFILE");
        assert_eq!(lake.tenant_prefix, "PKDUMP_TENANT_S3_PREFIX");
        assert_eq!(lake.config_relative_path, ".config/pkdump/lake.env");

        let keys = key_env();
        assert_eq!(keys.key_file_var, "PKDUMP_MASTER_KEY_FILE");
        assert_eq!(
            keys.default_relative_path,
            ".config/pkdump/tenant-master.key"
        );
        // The format tag and every HKDF label are what the master key already on disk and
        // every part already in the tenant zone were written under.
        assert_eq!(keys.format_tag(), "pkdump-master-key-v1:");
        assert_eq!(keys.label(), "pkdump");
    }
}

#[cfg(test)]
mod zone_agreement {
    //! The tenant zone's prefix and window live in three homes that cannot share code: the
    //! kit's Rust (the shipper reads them at runtime), the policy documents (AWS reads them) and
    //! the deploy script (bash). What they can share is this check. A prefix changed in one and
    //! not the others does not fail loudly — it silently widens a policy.
    //!
    //! It used to be a regex over `crates/pkdump-lake/src/tenant.rs` in a container-tier gate.
    //! The constants are the kit's now, so the check reads the COMPILED values rather than the
    //! text of a file this repository no longer holds, and runs in the unit tier.

    use dumpster_kit_lake::tenant::{CATALOG_ROOTS, RETENTION_DAYS, TENANT_ROOT};

    fn deploy(path: &str) -> String {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy");
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// Disjoint roots, or every containment assertion in the repo is about a boundary that does
    /// not exist: a `tenant/` moved under `lake/` would be inside the catalog's own prefix,
    /// governed by the catalog's indefinite-retention lifecycle rule and reachable by any
    /// credential granted the catalog, while every line of code and policy still said the two
    /// zones were separate.
    #[test]
    fn no_zone_root_contains_another() {
        let mut roots: Vec<(&str, &str)> = vec![("TENANT_ROOT", TENANT_ROOT)];
        roots.extend(CATALOG_ROOTS.iter().map(|r| ("CATALOG_ROOTS", *r)));
        assert!(roots.len() >= 2, "no roots to compare");
        for (an, a) in &roots {
            for (bn, b) in &roots {
                assert!(
                    (an, a) == (bn, b) || !b.starts_with(a),
                    "{an}={a:?} contains {bn}={b:?}"
                );
            }
        }
    }

    #[test]
    fn the_prefixes_and_the_window_agree_across_rust_the_policies_and_the_script() {
        assert_eq!(
            TENANT_ROOT, "tenant/",
            "this gate is written against tenant/"
        );
        assert_eq!(RETENTION_DAYS, 90, "90 is the product limit");

        let lifecycle = deploy("policies/tenant-zone/lifecycle.json");
        assert!(
            lifecycle.contains(&format!("\"Prefix\": \"{TENANT_ROOT}\"")),
            "lifecycle.json does not scope itself to {TENANT_ROOT}"
        );
        assert!(
            lifecycle.contains(&format!("\"Days\": {RETENTION_DAYS}")),
            "lifecycle.json does not expire after {RETENTION_DAYS} days"
        );

        let catalog = deploy("policies/tenant-zone/catalog-credentials.json");
        let tenant = deploy("policies/tenant-zone/tenant-credentials.json");
        let named = |root: &str| format!("{{{{BUCKET}}}}/{root}*");
        assert!(
            catalog.contains(&named(TENANT_ROOT)),
            "the catalog policy does not deny tenant/"
        );
        assert!(
            tenant.contains(&named(TENANT_ROOT)),
            "the tenant policy does not allow tenant/"
        );
        for c in CATALOG_ROOTS {
            assert!(
                catalog.contains(&named(c)),
                "the catalog policy does not allow {c}"
            );
            assert!(
                tenant.contains(&named(c)),
                "the tenant policy does not deny {c}"
            );
        }

        let script = deploy("setup-tenant-zone.sh");
        let lines: Vec<&str> = script.lines().collect();
        assert!(
            lines.contains(&format!("TENANT_PREFIX=\"{TENANT_ROOT}\"").as_str()),
            "setup-tenant-zone.sh disagrees with the kit about the zone prefix"
        );
        assert!(
            lines.contains(&format!("RETENTION_DAYS={RETENTION_DAYS}").as_str()),
            "setup-tenant-zone.sh disagrees with the kit about the retention window"
        );
        assert!(
            lines.contains(&format!("CATALOG_PREFIXES=\"{}\"", CATALOG_ROOTS.join(" ")).as_str()),
            "setup-tenant-zone.sh disagrees with the kit about the catalog prefixes"
        );
    }
}
