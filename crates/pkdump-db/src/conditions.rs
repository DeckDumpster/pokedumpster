//! Card-condition value multipliers (pokedumpster-e1vo).
//!
//! `data/conditions.json` is the canonical source for the TCGplayer raw-card
//! multipliers applied to a copy's Near-Mint market price to estimate its
//! value at its recorded condition. Reconciled into the `conditions` table at
//! `pkdump setup` / `open_shared`, served to the frontend via `/api/conditions`
//! (backing `$lib/conditions.svelte`), and read in Rust by the value-history
//! snapshot/backfill — so the multipliers live in exactly one place instead of
//! being duplicated in a TypeScript constant.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use crate::error::Result;

const CONDITIONS_SEED: &str = include_str!("../../../data/conditions.json");

/// One row of the `conditions` table. Mirrors the schema 1:1.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Condition {
    pub name: String,
    pub multiplier: f64,
    #[ts(type = "number")]
    pub rank: i64,
}

/// Re-seed `conditions` from `data/conditions.json`. Idempotent upsert.
pub fn reconcile(conn: &mut Connection) -> Result<usize> {
    let seed: Vec<Condition> = serde_json::from_str(CONDITIONS_SEED)?;
    let tx = conn.transaction()?;
    for c in &seed {
        tx.execute(
            "INSERT INTO conditions (name, multiplier, rank) VALUES (?1, ?2, ?3) \
             ON CONFLICT(name) DO UPDATE SET multiplier = excluded.multiplier, \
                                             rank = excluded.rank",
            params![c.name, c.multiplier, c.rank],
        )?;
    }
    tx.commit()?;
    Ok(seed.len())
}

/// Every condition, best first — for the API and the frontend store.
pub fn list_all(conn: &Connection) -> Result<Vec<Condition>> {
    let mut stmt = conn.prepare("SELECT name, multiplier, rank FROM conditions ORDER BY rank")?;
    let rows = stmt.query_map([], |r| {
        Ok(Condition {
            name: r.get(0)?,
            multiplier: r.get(1)?,
            rank: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `condition name → multiplier`, for value computations. Callers should
/// default an unknown/missing condition to `1.0` (treat as Near Mint) — the
/// same defensive default the frontend uses so a typo never zeroes a value.
pub fn multipliers(conn: &Connection) -> Result<HashMap<String, f64>> {
    Ok(list_all(conn)?
        .into_iter()
        .map(|c| (c.name, c.multiplier))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_shared;

    #[test]
    fn reconcile_seeds_the_five_conditions_with_multipliers() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        // open_shared already reconciled; re-running is idempotent.
        let n = reconcile(&mut conn).unwrap();
        assert_eq!(n, 5);

        let m = multipliers(&conn).unwrap();
        assert_eq!(m.get("Near Mint"), Some(&1.0));
        assert_eq!(m.get("Damaged"), Some(&0.25));
        assert_eq!(m.len(), 5);

        // Ordered best-first for display.
        let all = list_all(&conn).unwrap();
        assert_eq!(all.first().unwrap().name, "Near Mint");
        assert_eq!(all.last().unwrap().name, "Damaged");
    }
}
