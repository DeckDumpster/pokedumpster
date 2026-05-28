//! Logical-set views for "bundle" products like the Trick or Trade
//! BOOster Bundles. Bundles aren't pokemontcg.io sets — they're TCGCSV
//! groups whose products are reprints of cards from other sets, with a
//! distinguishing variant treatment (Halloween stamp, Cosmos Holo,
//! etc.). This module exposes them as 30-slot grids that let the user
//! enter a whole pack from one page instead of navigating to 30
//! individual cards.
//!
//! The slot→card resolution leans on `printings.tcgplayer_product_id`,
//! which the cross-group bridge in `pkdump-ingest` populates when it
//! attaches a TTBB product to its parent card. See pokedumpster-qfz.

use rusqlite::Connection;

use crate::error::{DbError, Result};

/// Static registry of bundle products we know how to render. Kept tiny
/// — there are exactly three TTBB bundles today; if a 2025 bundle ships
/// the appended row is the only edit.
const BUNDLES: &[(&str, &str, i64, i64)] = &[
    (
        "ttbb-2022",
        "Trick or Trade BOOster Bundle 2022",
        2022,
        3179,
    ),
    (
        "ttbb-2023",
        "Trick or Trade BOOster Bundle 2023",
        2023,
        23266,
    ),
    (
        "ttbb-2024",
        "Trick or Trade BOOster Bundle 2024",
        2024,
        23561,
    ),
];

/// One bundle's summary header, used for the index list.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Bundle {
    pub slug: String,
    pub name: String,
    #[ts(type = "number")]
    pub year: i64,
    #[ts(type = "number")]
    pub group_id: i64,
    /// Total products in the bundle (typically 30 for TTBB).
    #[ts(type = "number")]
    pub slot_count: i64,
    /// Number of products the user owns at least one copy of (via any
    /// printing tied to a product in the bundle's group).
    #[ts(type = "number")]
    pub owned_count: i64,
}

/// One product in a bundle, resolved to the parent card's printing
/// when the cross-group bridge linked them. When resolution fails the
/// product fields are still present but `printing_id` is `None` — the
/// UI shows the slot as "needs ingest fix" instead of letting the user
/// add the wrong thing.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BundleSlot {
    #[ts(type = "number")]
    pub product_id: i64,
    pub product_name: String,
    pub collector_number: String,
    pub image_url: Option<String>,
    /// Sortable numeric prefix of `collector_number` — driver of slot
    /// order in the grid.
    #[ts(type = "number")]
    pub number_sortable: i64,
    pub printing_id: Option<String>,
    pub card_id: Option<String>,
    pub card_name: Option<String>,
    pub set_code: Option<String>,
    pub set_name: Option<String>,
    pub number: Option<String>,
    pub variant: Option<String>,
    #[ts(type = "number")]
    pub owned_count: i64,
}

#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BundleDetail {
    pub bundle: Bundle,
    pub slots: Vec<BundleSlot>,
}

/// Every bundle the registry knows about, with current slot/own counts
/// rolled up. Cheap (one query per bundle, registry is small).
pub fn list_bundles(conn: &Connection) -> Result<Vec<Bundle>> {
    BUNDLES
        .iter()
        .map(|(slug, name, year, group_id)| {
            let (slot_count, owned_count) = bundle_counts(conn, *group_id)?;
            Ok(Bundle {
                slug: (*slug).to_string(),
                name: (*name).to_string(),
                year: *year,
                group_id: *group_id,
                slot_count,
                owned_count,
            })
        })
        .collect()
}

fn bundle_counts(conn: &Connection, group_id: i64) -> Result<(i64, i64)> {
    let slot_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tcgcsv_products WHERE group_id = ?1",
        [group_id],
        |r| r.get(0),
    )?;
    let owned_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT tp.product_id) \
           FROM tcgcsv_products tp \
           JOIN printings p ON p.tcgplayer_product_id = tp.product_id \
           JOIN collection co ON co.printing_id = p.printing_id \
          WHERE tp.group_id = ?1",
        [group_id],
        |r| r.get(0),
    )?;
    Ok((slot_count, owned_count))
}

/// The full 30-slot grid for a bundle slug, with each product resolved
/// to its parent printing (when the bridge linked one).
pub fn get_bundle(conn: &Connection, slug: &str) -> Result<Option<BundleDetail>> {
    let Some((slug_s, name, year, group_id)) = BUNDLES
        .iter()
        .find(|(s, _, _, _)| *s == slug)
        .map(|(s, n, y, g)| ((*s).to_string(), (*n).to_string(), *y, *g))
    else {
        return Ok(None);
    };

    let (slot_count, owned_count) = bundle_counts(conn, group_id)?;
    let bundle = Bundle {
        slug: slug_s,
        name,
        year,
        group_id,
        slot_count,
        owned_count,
    };

    let mut stmt = conn.prepare(
        "SELECT tp.product_id, tp.name, tp.collector_number, tp.image_url, \
                p.printing_id, c.card_id, c.name, c.set_code, s.name, c.number, p.variant, \
                COALESCE( \
                  (SELECT COUNT(*) FROM collection co \
                    WHERE co.printing_id = p.printing_id), 0) \
           FROM tcgcsv_products tp \
           LEFT JOIN printings p ON p.tcgplayer_product_id = tp.product_id \
           LEFT JOIN cards c ON c.card_id = p.card_id \
           LEFT JOIN sets s ON s.set_code = c.set_code \
          WHERE tp.group_id = ?1 \
          ORDER BY \
            CAST( \
              CASE WHEN INSTR(tp.collector_number, '/') > 0 \
                   THEN SUBSTR(tp.collector_number, 1, INSTR(tp.collector_number, '/') - 1) \
                   ELSE tp.collector_number END \
              AS INTEGER), \
            tp.product_id",
    )?;
    let slots: Vec<BundleSlot> = stmt
        .query_map([group_id], |r| {
            let collector_number: String = r.get(2)?;
            let number_sortable = collector_number
                .split('/')
                .next()
                .and_then(|s| s.trim_start_matches('0').parse::<i64>().ok())
                .unwrap_or(0);
            Ok(BundleSlot {
                product_id: r.get(0)?,
                product_name: r.get(1)?,
                collector_number,
                image_url: r.get(3)?,
                number_sortable,
                printing_id: r.get(4)?,
                card_id: r.get(5)?,
                card_name: r.get(6)?,
                set_code: r.get(7)?,
                set_name: r.get(8)?,
                number: r.get(9)?,
                variant: r.get(10)?,
                owned_count: r.get(11)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(BundleDetail { bundle, slots }))
}

/// Resolve a bundle slug to its TCGCSV group_id. Returns `NotFound`
/// for unknown slugs so callers can produce a 404.
pub fn group_id_for_slug(slug: &str) -> Result<i64> {
    BUNDLES
        .iter()
        .find(|(s, _, _, _)| *s == slug)
        .map(|(_, _, _, g)| *g)
        .ok_or_else(|| DbError::NotFound(format!("bundle {slug}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{self, NewCopy};
    use crate::{connect_user, open_shared};

    fn seed_bundle_ttbb24() -> (tempfile::TempDir, rusqlite::Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, printed_total) \
                 VALUES ('sv3', 'Obsidian Flames', 'Scarlet & Violet', 197)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                 VALUES ('sv3-130', 'sv3', '130', 130, 'Umbreon', 'Rare')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                 VALUES ('sv3-136', 'sv3', '136', 136, 'Darkrai', 'Rare')",
                [],
            )
            .unwrap();
            // Pre-ensure the variant codes so the FK on printings.variant
            // is satisfied (tests bypass expand_all_printings, so the
            // bulk-ensure that normally runs at function entry doesn't fire).
            for code in ["stamp_trick_or_trade", "cosmos_holo_trick_or_trade"] {
                crate::variants::ensure_code(&c, code).unwrap();
            }
            // Two TTBB 2024 products + their already-bridged printings.
            c.execute(
                "INSERT INTO tcgcsv_products \
                   (product_id, group_id, name, collector_number, derived_variant, image_url, fetched_at) \
                 VALUES (568704, 23561, 'Umbreon', '130/197', NULL, 'http://x/umb.jpg', '2026-05-26')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO tcgcsv_products \
                   (product_id, group_id, name, collector_number, derived_variant, image_url, fetched_at) \
                 VALUES (568826, 23561, 'Darkrai (Cosmos Holo)', '136/197', 'cosmos_holo', 'http://x/dark.jpg', '2026-05-26')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) \
                 VALUES ('sv3-130-stamp_trick_or_trade', 'sv3-130', 'stamp_trick_or_trade', 568704)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) \
                 VALUES ('sv3-136-cosmos_holo_trick_or_trade', 'sv3-136', 'cosmos_holo_trick_or_trade', 568826)",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn get_bundle_resolves_printings_and_orders_by_collector_number() {
        let (_d, conn) = seed_bundle_ttbb24();
        let detail = get_bundle(&conn, "ttbb-2024").unwrap().unwrap();
        assert_eq!(detail.bundle.year, 2024);
        assert_eq!(detail.bundle.group_id, 23561);
        assert_eq!(detail.slots.len(), 2);
        // Slot ordering is by numeric prefix of collector_number — 130
        // (Umbreon) before 136 (Darkrai).
        assert_eq!(detail.slots[0].number_sortable, 130);
        assert_eq!(
            detail.slots[0].variant.as_deref(),
            Some("stamp_trick_or_trade")
        );
        assert_eq!(detail.slots[0].card_id.as_deref(), Some("sv3-130"));
        assert_eq!(detail.slots[0].set_name.as_deref(), Some("Obsidian Flames"));
        assert_eq!(detail.slots[1].number_sortable, 136);
        assert_eq!(
            detail.slots[1].variant.as_deref(),
            Some("cosmos_holo_trick_or_trade")
        );
    }

    #[test]
    fn get_bundle_owned_counts_track_collection() {
        let (_d, mut conn) = seed_bundle_ttbb24();
        // Own one copy of the Umbreon stamp printing.
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3-130-stamp_trick_or_trade".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let detail = get_bundle(&conn, "ttbb-2024").unwrap().unwrap();
        let umb = detail
            .slots
            .iter()
            .find(|s| s.number_sortable == 130)
            .unwrap();
        let dark = detail
            .slots
            .iter()
            .find(|s| s.number_sortable == 136)
            .unwrap();
        assert_eq!(umb.owned_count, 1);
        assert_eq!(dark.owned_count, 0);
        assert_eq!(detail.bundle.owned_count, 1);
        assert_eq!(detail.bundle.slot_count, 2);
    }

    #[test]
    fn get_bundle_returns_none_for_unknown_slug() {
        let (_d, conn) = seed_bundle_ttbb24();
        assert!(get_bundle(&conn, "not-a-real-bundle").unwrap().is_none());
    }

    #[test]
    fn list_bundles_returns_all_registered_bundles() {
        let (_d, conn) = seed_bundle_ttbb24();
        let bundles = list_bundles(&conn).unwrap();
        assert_eq!(bundles.len(), 3);
        assert_eq!(bundles.iter().filter(|b| b.slot_count > 0).count(), 1);
        let ttbb24 = bundles.iter().find(|b| b.slug == "ttbb-2024").unwrap();
        assert_eq!(ttbb24.slot_count, 2);
    }
}
