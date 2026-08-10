//! Card-condition value multipliers (pokedumpster-e1vo).
//!
//! `data/conditions.json` is the canonical source for the TCGplayer raw-card
//! multipliers applied to a copy's Near-Mint market price to estimate its
//! value at its recorded condition. Served to the frontend via
//! `/api/conditions` (backing `$lib/conditions.svelte`), read by the
//! collection search's `order:value`, and read in Rust by the value-history
//! snapshot/backfill — so the multipliers live in exactly one place instead of
//! being duplicated in a TypeScript constant.
//!
//! The `conditions` table is **per tenant** (`schema_user.sql`), seeded by
//! [`seed_defaults`] on every [`crate::open_user`]. It moved out of the shared
//! catalog in pd-s4c2: `conditions.name` matches `collection.condition`, so
//! the table belongs beside the rows it is joined to rather than across the
//! ATTACH boundary from them.
//!
//! Seeding is **insert-if-absent, never an overwrite**. A tenant's five rows
//! are the tenant's own — the catalog is regenerated from upstream and can be
//! reconciled towards a seed file, a collection cannot. It is also what makes
//! re-opening an already-seeded collection write nothing at all, which every
//! open on a Litestream-replicated database has to (see the change-counter
//! tests in [`crate::connection`]).

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

/// Seed a collection's `conditions` with the five defaults from
/// `data/conditions.json`, filling in only the rows it does not already have.
///
/// Returns the number of rows inserted — zero on every open after the first,
/// which is the point: this runs on every [`crate::open_user`], and a
/// collection that already carries its multipliers must not be written to
/// merely for being opened.
///
/// Deliberately not an upsert. The rows belong to the tenant; re-applying the
/// seed file over them would be this build deciding it knows the multipliers
/// better than the collection does.
pub fn seed_defaults(conn: &Connection) -> Result<usize> {
    let seed: Vec<Condition> = serde_json::from_str(CONDITIONS_SEED)?;
    // `unchecked_transaction` so this can run on the shared `&Connection`
    // that `init_user_schema` is handed — the five inserts are one commit
    // either way, and nothing else holds a transaction open here.
    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0;
    for c in &seed {
        inserted += tx.execute(
            "INSERT INTO conditions (name, multiplier, rank) VALUES (?1, ?2, ?3) \
             ON CONFLICT(name) DO NOTHING",
            params![c.name, c.multiplier, c.rank],
        )?;
    }
    tx.commit()?;
    Ok(inserted)
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
    use crate::open_user;

    /// A brand-new collection is born with the five defaults — no `pkdump
    /// setup`, no catalog, nothing attached.
    #[test]
    fn a_new_collection_is_seeded_with_the_five_conditions() {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_user(&dir.path().join("collection.sqlite")).unwrap();

        let m = multipliers(&conn).unwrap();
        assert_eq!(m.get("Near Mint"), Some(&1.0));
        assert_eq!(m.get("Lightly Played"), Some(&0.85));
        assert_eq!(m.get("Moderately Played"), Some(&0.65));
        assert_eq!(m.get("Heavily Played"), Some(&0.45));
        assert_eq!(m.get("Damaged"), Some(&0.25));
        assert_eq!(m.len(), 5);

        // Ordered best-first for display.
        let all = list_all(&conn).unwrap();
        assert_eq!(all.first().unwrap().name, "Near Mint");
        assert_eq!(all.last().unwrap().name, "Damaged");
    }

    /// The seed runs on every open, so the second one must insert nothing.
    #[test]
    fn seeding_an_already_seeded_collection_inserts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        let conn = open_user(&path).unwrap();
        assert_eq!(seed_defaults(&conn).unwrap(), 0);
    }

    /// Insert-if-absent, not upsert: a multiplier the tenant carries is not
    /// this build's to replace on the next open. (Editing is a separate,
    /// deliberately unbuilt feature — this pins the seed's behaviour so the
    /// day it lands the seed does not quietly undo it.)
    #[test]
    fn seeding_never_overwrites_a_multiplier_the_collection_already_holds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        {
            let conn = open_user(&path).unwrap();
            conn.execute(
                "UPDATE conditions SET multiplier = 0.5 WHERE name = 'Near Mint'",
                [],
            )
            .unwrap();
        }

        let conn = open_user(&path).unwrap();
        assert_eq!(multipliers(&conn).unwrap().get("Near Mint"), Some(&0.5));
    }

    /// A row that went missing comes back — the seed fills gaps, which is
    /// what makes it a migration for a collection that predates the table
    /// as well as the birth state of a new one.
    #[test]
    fn a_missing_condition_is_restored_on_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("collection.sqlite");
        {
            let conn = open_user(&path).unwrap();
            conn.execute("DELETE FROM conditions WHERE name = 'Damaged'", [])
                .unwrap();
        }

        let conn = open_user(&path).unwrap();
        assert_eq!(multipliers(&conn).unwrap().get("Damaged"), Some(&0.25));
    }
}
