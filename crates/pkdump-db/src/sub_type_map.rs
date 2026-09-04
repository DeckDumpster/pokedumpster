//! Group-aware lookup from TCGCSV `prices.sub_type_name` to a PokeDumpster
//! variant code. `data/tcgcsv_sub_type_variants.json` is the canonical
//! authoring source; `reconcile()` re-applies it on every `pkdump setup`
//! before variant expansion runs (mirrors `variants::reconcile`).
//!
//! Why group-aware: TCGCSV models Base Set as two groups — 604 (Unlimited,
//! with shadow) and 1663 (Shadowless) — and the same sub_type string
//! ("Unlimited Holofoil") means physically different cards in each. The
//! flat Rust match this replaces could only ever pick one meaning. See
//! pokedumpster-5is.
//!
//! Resolution order at lookup time:
//!   1. (group_id, sub_type) — exact group-specific row.
//!   2. (0, sub_type) — global default. Covers every modern set.
//!   3. None — caller drops the price row (same semantics the old flat
//!      mapper had for unknown sub_types).
//!
//! Group id 0 is reserved as the global sentinel; real TCGCSV group ids
//! start at 1.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use serde::Deserialize;

use crate::error::Result;

pub(crate) const SUB_TYPE_VARIANTS_SEED: &str =
    include_str!("../../../data/tcgcsv_sub_type_variants.json");

/// Sentinel group id used to register a row in the global default map.
/// Real TCGCSV group ids start at 1, so this can never collide.
pub const GLOBAL_GROUP_ID: i64 = 0;

#[derive(Debug, Clone, Deserialize)]
struct SeedGroup {
    tcgcsv_group_id: i64,
    entries: Vec<SeedEntry>,
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SeedEntry {
    sub_type: String,
    variant: String,
}

/// In-memory copy of `tcgcsv_sub_type_variant_map` for use during variant
/// expansion. Loaded once at the top of `expand_all_printings`, queried
/// per-product thereafter.
#[derive(Debug, Default, Clone)]
pub struct SubTypeVariantMap {
    by_group: HashMap<(i64, String), String>,
}

impl SubTypeVariantMap {
    /// Load every row from `tcgcsv_sub_type_variant_map`.
    pub fn load(conn: &Connection) -> Result<Self> {
        let mut stmt = conn.prepare(
            "SELECT tcgcsv_group_id, sub_type_name, variant_code \
               FROM tcgcsv_sub_type_variant_map",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut by_group: HashMap<(i64, String), String> = HashMap::new();
        for row in rows {
            let (g, s, v) = row?;
            by_group.insert((g, s), v);
        }
        Ok(Self { by_group })
    }

    /// Resolve a `(group_id, sub_type)` pair to a variant code. Returns
    /// `None` when no group-specific row exists AND no global default
    /// row exists — caller drops the price row.
    pub fn lookup(&self, group_id: i64, sub_type: &str) -> Option<&str> {
        if let Some(v) = self.by_group.get(&(group_id, sub_type.to_string())) {
            return Some(v.as_str());
        }
        self.by_group
            .get(&(GLOBAL_GROUP_ID, sub_type.to_string()))
            .map(String::as_str)
    }
}

/// Re-seed `tcgcsv_sub_type_variant_map` from
/// `data/tcgcsv_sub_type_variants.json`. Idempotent — call from
/// `pkdump setup` after `variants::reconcile` (so the FK on
/// `variant_code` is satisfied) and before `expand_all_printings`.
///
/// Returns the number of rows written.
pub fn reconcile(conn: &mut Connection) -> Result<usize> {
    let seed: Vec<SeedGroup> = serde_json::from_str(SUB_TYPE_VARIANTS_SEED)?;
    let tx = conn.transaction()?;
    // Clear-then-rewrite: the JSON is the source of truth; we don't want
    // stale rows from earlier authoring lingering in the table.
    tx.execute("DELETE FROM tcgcsv_sub_type_variant_map", [])?;
    let mut n = 0;
    for group in &seed {
        for entry in &group.entries {
            tx.execute(
                "INSERT INTO tcgcsv_sub_type_variant_map \
                   (tcgcsv_group_id, sub_type_name, variant_code) \
                 VALUES (?1, ?2, ?3)",
                params![group.tcgcsv_group_id, entry.sub_type, entry.variant],
            )?;
            n += 1;
        }
    }
    tx.commit()?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_shared;

    fn fresh() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        // reconcile variants first — the sub_type_map FK references variants.code.
        let mut c = conn;
        crate::variants::reconcile(&mut c).unwrap();
        (dir, c)
    }

    #[test]
    fn reconcile_loads_seed_and_is_idempotent() {
        let (_d, mut conn) = fresh();
        let n1 = reconcile(&mut conn).unwrap();
        let n2 = reconcile(&mut conn).unwrap();
        assert_eq!(n1, n2, "row count stable on re-run");
        assert!(n1 > 0, "seed produced at least one row");
    }

    #[test]
    fn lookup_falls_back_to_global_default() {
        let (_d, mut conn) = fresh();
        reconcile(&mut conn).unwrap();
        let map = SubTypeVariantMap::load(&conn).unwrap();
        // Modern set group with no explicit override → resolves via
        // group_id=0 fallback.
        assert_eq!(map.lookup(99999, "Holofoil"), Some("holo"));
        assert_eq!(map.lookup(99999, "Reverse Holofoil"), Some("reverse_holo"));
    }

    #[test]
    fn lookup_prefers_group_specific_row_over_global() {
        let (_d, mut conn) = fresh();
        reconcile(&mut conn).unwrap();
        let map = SubTypeVariantMap::load(&conn).unwrap();
        // Group 1663 (Base Set Shadowless) overrides the meaning of
        // "Unlimited Holofoil" — the global default says unlimited_holo
        // (Jungle/Fossil etc.), but in the Shadowless group it means
        // a card without the right-side art-frame shadow.
        assert_eq!(
            map.lookup(0, "Unlimited Holofoil"),
            Some("unlimited_holo"),
            "global default keeps unlimited_holo for the WotC-era non-Base groups"
        );
        assert_eq!(
            map.lookup(1663, "Unlimited Holofoil"),
            Some("shadowless_holo"),
            "group 1663 reroutes Unlimited Holofoil to the shadowless treatment"
        );
        assert_eq!(
            map.lookup(1663, "Unlimited"),
            Some("shadowless_normal"),
            "and the non-holo equivalent"
        );
    }

    #[test]
    fn jungle_group_recognizes_plain_unlimited_and_first_edition() {
        // The flat Rust mapper this replaces returned None for plain
        // "1st Edition" / "Unlimited" (no Holofoil suffix), so Jungle
        // commons silently lost their first_ed_normal / unlimited_normal
        // printings. The group-aware table picks them up.
        let (_d, mut conn) = fresh();
        reconcile(&mut conn).unwrap();
        let map = SubTypeVariantMap::load(&conn).unwrap();
        assert_eq!(map.lookup(635, "1st Edition"), Some("first_ed_normal"));
        assert_eq!(map.lookup(635, "Unlimited"), Some("unlimited_normal"));
    }

    #[test]
    fn unknown_sub_type_returns_none() {
        let (_d, mut conn) = fresh();
        reconcile(&mut conn).unwrap();
        let map = SubTypeVariantMap::load(&conn).unwrap();
        assert_eq!(map.lookup(604, "Holographic Diamond Foil"), None);
    }
}
