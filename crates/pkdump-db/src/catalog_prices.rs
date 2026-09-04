//! Hand-curated prices for catalog printings TCGplayer does not price
//! (pd-m4gw).
//!
//! `data/overrides/catalog_prices.json` is the canonical source, reconciled
//! into the shared `catalog_price_overrides` table at `pkdump setup` /
//! [`crate::open_shared`] time — the same curated-patch layer as
//! `data/overrides/variant_augmentations.json`.
//!
//! **Why the catalog and not the tenant.** These prices used to be
//! hand-entered per user into the tenant's `manual_prices`. That was the
//! wrong home twice over: a printing the feed cannot price is missing for
//! *every* tenant, so each one had to rediscover the same gap; and ordering
//! the collection by price then had to `COALESCE` across the `ATTACH`
//! boundary, which no index can satisfy. Both problems disappear once the
//! override sits beside `latest_prices` in `shared`.
//!
//! **Why a seed file and not a write path.** `shared.sqlite` is
//! reproducible-from-upstream and is deliberately not replicated off-box
//! (only the tenant collection is — see `deploy/RESTORE.md`). A price that
//! existed only inside it would be destroyed by a catalog rebuild or a
//! restore with nothing to notice the loss. In git it survives both.
//!
//! **Precedence.** `latest_prices` always wins; an override is consulted
//! only where the feed has nothing (see [`crate::prices::MARKET_PRICE_EXPR`]).
//! So an override left behind after upstream starts pricing the printing is
//! inert, not wrong — which is what makes it safe to keep one until a
//! refresh has actually proved the gap closed.

use rusqlite::{Connection, params};

use crate::error::Result;

/// `data/overrides/catalog_prices.json` — the curated override list.
pub(crate) const CATALOG_PRICES_SEED: &str =
    include_str!("../../../data/overrides/catalog_prices.json");

/// One curated catalog price. Mirrors the table 1:1.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct CatalogPriceOverride {
    pub printing_id: String,
    pub price: f64,
    pub observed_at: String,
    pub note: Option<String>,
}

/// Re-seed `catalog_price_overrides` from the JSON. Idempotent upsert.
///
/// A row whose printing is absent from *this* catalog is skipped rather than
/// tripping the FK — a minimal test catalog carries neither `basep` nor most
/// of the sets the real one does, exactly as [`crate::set_aliases::reconcile`]
/// handles the same case. Returns the number of overrides actually written.
pub fn reconcile(conn: &mut Connection) -> Result<usize> {
    let seed: Vec<CatalogPriceOverride> = serde_json::from_str(CATALOG_PRICES_SEED)?;
    let tx = conn.transaction()?;
    let mut written = 0usize;
    for o in &seed {
        written += tx.execute(
            "INSERT INTO catalog_price_overrides (printing_id, price, observed_at, note) \
             SELECT ?1, ?2, ?3, ?4 \
              WHERE EXISTS (SELECT 1 FROM printings WHERE printing_id = ?1) \
             ON CONFLICT(printing_id) DO UPDATE SET price       = excluded.price, \
                                                    observed_at = excluded.observed_at, \
                                                    note        = excluded.note",
            params![o.printing_id, o.price, o.observed_at, o.note],
        )?;
    }
    tx.commit()?;
    Ok(written)
}

/// Whether a printing has a curated catalog override.
pub fn has_override(conn: &Connection, printing_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM catalog_price_overrides WHERE printing_id = ?1)",
        params![printing_id],
        |r| r.get::<_, i64>(0),
    )? == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_shared;

    fn catalog_with_basep(dir: &std::path::Path) -> Connection {
        let conn = open_shared(&dir.join("shared.sqlite")).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series) \
             VALUES ('basep', 'Wizards Black Star Promos', 'Base')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
             VALUES ('basep-10', 'basep', '10', 10, 'Meowth')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO printings (printing_id, card_id, variant) \
             VALUES ('basep-10-normal', 'basep-10', 'normal')",
            [],
        )
        .unwrap();
        conn
    }

    #[test]
    fn reconcile_skips_overrides_whose_printing_this_catalog_lacks() {
        let dir = tempfile::tempdir().unwrap();
        // A fresh catalog has no printings at all — every seed row is
        // skipped rather than failing the FK.
        let mut conn = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        assert_eq!(reconcile(&mut conn).unwrap(), 0);
    }

    #[test]
    fn reconcile_is_idempotent_and_writes_present_printings() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = catalog_with_basep(dir.path());

        assert_eq!(reconcile(&mut conn).unwrap(), 1);
        assert!(has_override(&conn, "basep-10-normal").unwrap());
        assert!(!has_override(&conn, "basep-14-normal").unwrap());

        // Second pass writes the same row again, and there is still one.
        reconcile(&mut conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM catalog_price_overrides", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn reconcile_overwrites_a_locally_edited_price() {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = catalog_with_basep(dir.path());
        reconcile(&mut conn).unwrap();
        conn.execute(
            "UPDATE catalog_price_overrides SET price = 1.0 WHERE printing_id = 'basep-10-normal'",
            [],
        )
        .unwrap();

        // The seed file is the source of truth — the catalog converges back
        // to it, which is what makes the JSON the thing to edit.
        reconcile(&mut conn).unwrap();
        let p: f64 = conn
            .query_row(
                "SELECT price FROM catalog_price_overrides WHERE printing_id = 'basep-10-normal'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(p, 61.0);
    }

    #[test]
    fn every_seeded_override_names_a_printing_and_a_positive_price() {
        let seed: Vec<CatalogPriceOverride> = serde_json::from_str(CATALOG_PRICES_SEED).unwrap();
        for o in &seed {
            assert!(
                o.printing_id.contains('-'),
                "not a printing_id: {}",
                o.printing_id
            );
            assert!(o.price > 0.0, "non-positive price for {}", o.printing_id);
            assert!(
                chrono::DateTime::parse_from_rfc3339(&o.observed_at).is_ok(),
                "observed_at not RFC3339 for {}: {}",
                o.printing_id,
                o.observed_at
            );
        }
    }

    /// The two `base2-*-user-1` rows are printings a user invented; they are
    /// absent from the shared `printings` table entirely. Putting one here
    /// would expose one tenant's invented printing to every other tenant —
    /// a tenant-isolation regression rather than a cleanup (pd-m4gw).
    #[test]
    fn seed_carries_no_user_created_printing() {
        let seed: Vec<CatalogPriceOverride> = serde_json::from_str(CATALOG_PRICES_SEED).unwrap();
        for o in &seed {
            assert!(
                !o.printing_id.contains("-user-"),
                "user-created printing in the shared seed: {}",
                o.printing_id
            );
        }
    }
}
