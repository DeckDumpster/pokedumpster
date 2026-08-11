//! Logical-set "containers" for bundle products like the Trick or Trade
//! BOOster Bundles. Bundles aren't pokemontcg.io sets — they're TCGCSV
//! groups whose products are reprints of cards from other sets, with a
//! distinguishing variant treatment (Halloween stamp, Cosmos Holo, etc.).
//!
//! The bundle registry is data: `data/bundles.json` is the canonical
//! authoring source, seeded into the `bundles` table at `pkdump setup`
//! time by [`reconcile`]. Bundles project into the same [`SetSummary`]
//! and [`BinderPage`] shapes that real sets use (pokedumpster-80q), so
//! the `/browse` picker and `/browse/[code]` page render both kinds
//! through the same UI — kind-discriminated by `"bundle"`.
//!
//! The slot→card resolution leans on `printings.tcgplayer_product_id`,
//! which the cross-group bridge in `pkdump-ingest` populates when it
//! attaches a TTBB product to its parent card. See pokedumpster-qfz.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};

use crate::binder::{
    BinderPage, BinderQuery, BinderSetInfo, BinderSlot, ExternalSet, SlotPrinting,
};
use crate::error::Result;
use crate::search_meta::RarityLookup;
use crate::sets::{self, CardCopyCount, SetAnalytics, SetSummary};

/// `data/bundles.json` — the canonical bundle registry.
const BUNDLES_SEED: &str = include_str!("../../../data/bundles.json");

/// One registry row, mirrors the `bundles` table 1:1.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Bundle {
    pub slug: String,
    pub name: String,
    pub year: i64,
    pub tcgcsv_group_id: i64,
    #[serde(default = "default_series")]
    pub series: String,
}

fn default_series() -> String {
    "Trick or Trade Bundle".to_string()
}

/// Re-seed `bundles` from `data/bundles.json`. Called by `pkdump setup`
/// (and `data refresh`) before anything consults the table.
pub fn reconcile(conn: &mut Connection) -> Result<usize> {
    let seed: Vec<Bundle> = serde_json::from_str(BUNDLES_SEED)?;
    let tx = conn.transaction()?;
    for b in &seed {
        tx.execute(
            "INSERT INTO bundles (slug, name, year, tcgcsv_group_id, series) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(slug) DO UPDATE SET \
                name = excluded.name, \
                year = excluded.year, \
                tcgcsv_group_id = excluded.tcgcsv_group_id, \
                series = excluded.series",
            params![b.slug, b.name, b.year, b.tcgcsv_group_id, b.series],
        )?;
    }
    tx.commit()?;
    Ok(seed.len())
}

/// Read every bundle in the registry.
pub fn list_bundles(conn: &Connection) -> Result<Vec<Bundle>> {
    let mut stmt = conn.prepare(
        "SELECT slug, name, year, tcgcsv_group_id, series \
           FROM bundles ORDER BY year DESC, slug",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Bundle {
            slug: r.get(0)?,
            name: r.get(1)?,
            year: r.get(2)?,
            tcgcsv_group_id: r.get(3)?,
            series: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Cheap existence check used by the `/api/sets/{code}/*` dispatch.
pub fn is_bundle(conn: &Connection, slug: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM bundles WHERE slug = ?1",
        [slug],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// (slot count, owned-slot count) for a bundle's tcgcsv group.
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

/// Project every bundle into a [`SetSummary`] so `/api/sets` can return
/// bundles + real sets in one list. `release_date` is synthesized as
/// October 1st of the bundle year (TTBB ships in October) so the
/// release-date sort lands them in the right rough position.
pub fn list_bundle_summaries(conn: &Connection) -> Result<Vec<SetSummary>> {
    let mut out = Vec::new();
    for b in list_bundles(conn)? {
        let (slot_count, owned_count) = bundle_counts(conn, b.tcgcsv_group_id)?;
        out.push(SetSummary {
            set_code: b.slug,
            ptcgo_code: None,
            name: b.name,
            series: b.series,
            total: Some(slot_count),
            printed_total: None,
            release_date: Some(format!("{}-10-01", b.year)),
            logo_url: None,
            symbol_url: None,
            total_cards: slot_count,
            owned_cards: owned_count,
            // Bundles have no base/secret/subset/promo split — the base
            // bar in the picker hides on null.
            base_total_cards: None,
            base_owned_cards: None,
            kind: "bundle".to_string(),
            // A bundle is defined by data/bundles.json, not by upstream —
            // there is nothing for pokemontcg.io to supersede.
            synthesized: false,
        });
    }
    Ok(out)
}

/// One bundle product as it comes back from the catalog join. Internal
/// to slot assembly — projected into a [`BinderSlot`] below.
struct BundleProductRow {
    product_id: i64,
    product_name: String,
    product_image: Option<String>,
    number_sortable: i64,
    printing_id: Option<String>,
    variant: Option<String>,
    deprecated_at: Option<String>,
    market_price: Option<f64>,
    card_id: Option<String>,
    card_name: Option<String>,
    card_number: Option<String>,
    card_rarity: Option<String>,
    card_image_large: Option<String>,
    home_set_code: Option<String>,
    home_set_name: Option<String>,
    owned_count: i64,
}

/// Resolve a bundle's product rows in collector-number order.
fn fetch_bundle_products(conn: &Connection, group_id: i64) -> Result<Vec<BundleProductRow>> {
    let mut stmt = conn.prepare(concat!(
        "SELECT tp.product_id, tp.name, tp.image_url, \
                tp.collector_number, \
                p.printing_id, p.variant, p.deprecated_at, \
                ",
        crate::market_price_expr!(),
        ", \
                c.card_id, c.name, c.number, c.rarity, c.image_large, \
                s.set_code, s.name, \
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
    ))?;
    let rows = stmt.query_map([group_id], |r| {
        let collector_number: String = r.get(3)?;
        let number_sortable = collector_number
            .split('/')
            .next()
            .and_then(|s| s.trim_start_matches('0').parse::<i64>().ok())
            .unwrap_or(0);
        Ok(BundleProductRow {
            product_id: r.get(0)?,
            product_name: r.get(1)?,
            product_image: r.get(2)?,
            number_sortable,
            printing_id: r.get(4)?,
            variant: r.get(5)?,
            deprecated_at: r.get(6)?,
            market_price: r.get(7)?,
            card_id: r.get(8)?,
            card_name: r.get(9)?,
            card_number: r.get(10)?,
            card_rarity: r.get(11)?,
            card_image_large: r.get(12)?,
            home_set_code: r.get(13)?,
            home_set_name: r.get(14)?,
            owned_count: r.get(15)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Project a single bundle product row into a [`BinderSlot`]. Resolved
/// slots get the underlying card's name/rarity/image and a one-printing
/// `printings` list (the TTBB variant); unresolved slots fall back to
/// the raw product fields and carry no printings, so the slot still
/// renders but the "+" affordance is disabled in the modal.
fn slot_from_product(row: BundleProductRow) -> BinderSlot {
    let mut printings = Vec::new();
    if let (Some(printing_id), Some(variant)) = (row.printing_id, row.variant) {
        printings.push(SlotPrinting {
            printing_id,
            variant,
            deprecated: row.deprecated_at.is_some(),
            owned_count: row.owned_count,
            market_price: row.market_price,
        });
    }
    let external_set = match (row.home_set_code, row.home_set_name) {
        (Some(set_code), Some(name)) => Some(ExternalSet { set_code, name }),
        _ => None,
    };
    BinderSlot {
        // Stable synthetic id when no card is bridged yet so the slot
        // still has a key for the grid render.
        card_id: row
            .card_id
            .unwrap_or_else(|| format!("bundle-product-{}", row.product_id)),
        number: row
            .card_number
            .unwrap_or_else(|| row.number_sortable.to_string()),
        name: row.card_name.unwrap_or(row.product_name),
        rarity: row.card_rarity,
        // Prefer the card's catalog image (the artwork the user expects to
        // recognize) over the product thumbnail — the TTBB variant pip
        // already tells them which finish this is.
        image_large: row.card_image_large.or(row.product_image),
        section: "base".to_string(),
        printings,
        external_set,
    }
}

/// A slot's price for sort — dearest market price across its printings.
fn slot_price(slot: &BinderSlot) -> f64 {
    slot.printings
        .iter()
        .filter_map(|p| p.market_price)
        .fold(0.0_f64, f64::max)
}

/// Whether the user owns at least one copy of a slot's printing.
fn owns_one(slot: &BinderSlot) -> bool {
    slot.printings.iter().any(|p| p.owned_count > 0)
}

/// Assemble a [`BinderPage`] for `slug`, mirroring the contract of
/// [`crate::binder::get_binder_page`]. `None` if the slug isn't a
/// registered bundle. `include_secret/subset/promos` are accepted but
/// ignored — bundles have a single section.
pub fn get_bundle_binder(
    conn: &Connection,
    slug: &str,
    q: &BinderQuery,
) -> Result<Option<BinderPage>> {
    let bundle: Option<Bundle> = conn
        .prepare(
            "SELECT slug, name, year, tcgcsv_group_id, series \
               FROM bundles WHERE slug = ?1",
        )?
        .query_row([slug], |r| {
            Ok(Bundle {
                slug: r.get(0)?,
                name: r.get(1)?,
                year: r.get(2)?,
                tcgcsv_group_id: r.get(3)?,
                series: r.get(4)?,
            })
        })
        .optional()?;
    let Some(bundle) = bundle else {
        return Ok(None);
    };

    let rows = fetch_bundle_products(conn, bundle.tcgcsv_group_id)?;
    let mut visible: Vec<BinderSlot> = rows.into_iter().map(slot_from_product).collect();

    // Master-set progress: every slot counts; "owned" = bridged + the
    // user has at least one copy of the bridged printing.
    let base_total = visible.len() as i64;
    let base_owned = visible.iter().filter(|s| owns_one(s)).count() as i64;
    // For bundles, master == base — there's no separate master/printing
    // axis, every slot has exactly one canonical printing.
    let master_total = base_total;
    let master_owned = base_owned;

    let set = BinderSetInfo {
        set_code: bundle.slug,
        name: bundle.name,
        series: bundle.series,
        total: Some(base_total),
        printed_total: None,
        kind: "bundle".to_string(),
    };

    // Sort.
    match q.sort.as_str() {
        "number_desc" => visible.reverse(),
        "name" => visible.sort_by(|a, b| {
            a.name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase())
        }),
        "name_desc" => visible.sort_by(|a, b| {
            b.name
                .to_ascii_lowercase()
                .cmp(&a.name.to_ascii_lowercase())
        }),
        "price" => visible.sort_by(|a, b| slot_price(b).total_cmp(&slot_price(a))),
        "price_asc" => visible.sort_by(|a, b| slot_price(a).total_cmp(&slot_price(b))),
        _ => {} // "number" — already ascending by collector_number prefix.
    }

    // Search + ownership filter.
    let search = q.search.trim().to_ascii_lowercase();
    visible.retain(|s| {
        if !search.is_empty() && !s.name.to_ascii_lowercase().contains(&search) {
            return false;
        }
        match q.filter.as_str() {
            "have" => owns_one(s),
            "need" => !owns_one(s),
            "dupes" => s.printings.iter().any(|p| p.owned_count >= 2),
            _ => true,
        }
    });

    // Paginate.
    let layout = q.layout.clamp(1, 60);
    let visible_count = visible.len() as i64;
    let total_pages = ((visible_count + layout - 1) / layout).max(1);
    let page = q.page.clamp(1, total_pages);
    let start = ((page - 1) * layout) as usize;
    let slots: Vec<BinderSlot> = visible
        .into_iter()
        .skip(start)
        .take(layout as usize)
        .collect();

    Ok(Some(BinderPage {
        set,
        layout,
        page,
        total_pages,
        base_total,
        base_owned,
        master_total,
        master_owned,
        slots,
    }))
}

/// Project a bundle into the [`SetAnalytics`] shape so the
/// `/browse/{slug}/stats` page works for bundles too. Degraded vs. real
/// sets: rarity split and copy-count histogram are derived from bridged
/// cards where possible; market value is the sum of bridged-printing
/// market prices.
pub fn analytics(conn: &Connection, slug: &str) -> Result<Option<SetAnalytics>> {
    let bundle: Option<Bundle> = conn
        .prepare(
            "SELECT slug, name, year, tcgcsv_group_id, series \
               FROM bundles WHERE slug = ?1",
        )?
        .query_row([slug], |r| {
            Ok(Bundle {
                slug: r.get(0)?,
                name: r.get(1)?,
                year: r.get(2)?,
                tcgcsv_group_id: r.get(3)?,
                series: r.get(4)?,
            })
        })
        .optional()?;
    let Some(bundle) = bundle else {
        return Ok(None);
    };

    let rows = fetch_bundle_products(conn, bundle.tcgcsv_group_id)?;

    // Totals over slots.
    let total_cards = rows.len() as i64;
    let owned_cards = rows.iter().filter(|r| r.owned_count > 0).count() as i64;
    let total_printings = rows.iter().filter(|r| r.printing_id.is_some()).count() as i64;
    let owned_printings = rows.iter().filter(|r| r.owned_count > 0).count() as i64;
    let owned_copies: i64 = rows.iter().map(|r| r.owned_count).sum();

    // Value: sum across resolved printings using the same market-price
    // resolver as the binder query.
    let market_value: f64 = rows.iter().filter_map(|r| r.market_price).sum();
    let owned_value_unique: f64 = rows
        .iter()
        .filter(|r| r.owned_count > 0)
        .filter_map(|r| r.market_price)
        .sum();
    let owned_value: f64 = rows
        .iter()
        .filter_map(|r| r.market_price.map(|p| p * r.owned_count as f64))
        .sum();

    // Rarity split from underlying cards. Same catalog typology the
    // per-set breakdown uses — a bundle's stats page is the same page.
    let tiers = RarityLookup::load(conn)?;
    let mut rarity_totals: HashMap<String, (i64, i64)> = HashMap::new();
    for r in &rows {
        if let Some(rarity) = &r.card_rarity {
            let entry = rarity_totals.entry(rarity.clone()).or_insert((0, 0));
            entry.0 += 1;
            if r.owned_count > 0 {
                entry.1 += 1;
            }
        }
    }
    let rarities = sets::rank_rarities(
        &tiers,
        rarity_totals
            .into_iter()
            .map(|(rarity, (t, o))| (rarity, t, o))
            .collect(),
    );

    // Per-slot copy counts. Sums every printing's copies (one printing
    // per slot for bundles, so this is just the slot's owned_count).
    let copy_counts: Vec<CardCopyCount> = rows
        .iter()
        .map(|r| CardCopyCount {
            number: r
                .card_number
                .clone()
                .unwrap_or(r.number_sortable.to_string()),
            number_sortable: r.number_sortable,
            rarity: r.card_rarity.as_deref().map(|s| tiers.display(s)),
            rarity_grp: r.card_rarity.as_deref().and_then(|s| tiers.grp(s)),
            copies: r.owned_count,
        })
        .collect();

    Ok(Some(SetAnalytics {
        set_code: bundle.slug,
        name: bundle.name,
        series: bundle.series,
        // Bundles have no base/secret split — base equals master.
        base_total_cards: total_cards,
        base_owned_cards: owned_cards,
        total_cards,
        owned_cards,
        total_printings,
        owned_printings,
        owned_copies,
        market_value,
        owned_value_unique,
        owned_value,
        rarities,
        copy_counts,
    }))
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
            let mut c = open_shared(&shared).unwrap();
            // The migrations create empty `bundles` — seed from JSON so
            // is_bundle/get_bundle_binder/analytics can find ttbb-2024.
            reconcile(&mut c).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, printed_total) \
                 VALUES ('sv3', 'Obsidian Flames', 'Scarlet & Violet', 197)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity, image_large) \
                 VALUES ('sv3-130', 'sv3', '130', 130, 'Umbreon', 'Rare', 'http://x/umb-card.jpg')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity, image_large) \
                 VALUES ('sv3-136', 'sv3', '136', 136, 'Darkrai', 'Rare', 'http://x/dark-card.jpg')",
                [],
            )
            .unwrap();
            for code in ["stamp_trick_or_trade", "cosmos_holo_trick_or_trade"] {
                crate::variants::ensure_code(&c, code).unwrap();
            }
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
    fn reconcile_seeds_registry_from_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        let n = reconcile(&mut c).unwrap();
        assert!(n >= 3, "expected at least 3 bundles seeded, got {n}");
        let bundles = list_bundles(&c).unwrap();
        assert!(bundles.iter().any(|b| b.slug == "ttbb-2024"));
        // Re-running is idempotent.
        let n2 = reconcile(&mut c).unwrap();
        assert_eq!(n, n2);
    }

    #[test]
    fn is_bundle_recognizes_seeded_slugs() {
        let (_d, conn) = seed_bundle_ttbb24();
        assert!(is_bundle(&conn, "ttbb-2024").unwrap());
        assert!(!is_bundle(&conn, "sv3").unwrap());
    }

    #[test]
    fn binder_resolves_printings_and_orders_by_collector_number() {
        let (_d, conn) = seed_bundle_ttbb24();
        let page = get_bundle_binder(&conn, "ttbb-2024", &BinderQuery::default())
            .unwrap()
            .unwrap();
        assert_eq!(page.set.kind, "bundle");
        assert_eq!(page.slots.len(), 2);
        assert_eq!(page.slots[0].name, "Umbreon");
        assert_eq!(page.slots[0].external_set.as_ref().unwrap().set_code, "sv3");
        assert_eq!(page.slots[0].printings.len(), 1);
        assert_eq!(page.slots[0].printings[0].variant, "stamp_trick_or_trade");
        assert_eq!(page.slots[1].name, "Darkrai");
        assert_eq!(page.base_total, 2);
        assert_eq!(page.master_total, 2);
    }

    #[test]
    fn binder_owned_counts_track_collection_and_filters_have() {
        let (_d, mut conn) = seed_bundle_ttbb24();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3-130-stamp_trick_or_trade".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let page = get_bundle_binder(&conn, "ttbb-2024", &BinderQuery::default())
            .unwrap()
            .unwrap();
        assert_eq!(page.base_owned, 1);
        // "have" tab keeps only the owned slot.
        let have = get_bundle_binder(
            &conn,
            "ttbb-2024",
            &BinderQuery {
                filter: "have".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(have.slots.len(), 1);
        assert_eq!(have.slots[0].name, "Umbreon");
    }

    #[test]
    fn list_summaries_projects_bundles_with_kind() {
        let (_d, conn) = seed_bundle_ttbb24();
        let summaries = list_bundle_summaries(&conn).unwrap();
        let ttbb24 = summaries
            .iter()
            .find(|s| s.set_code == "ttbb-2024")
            .unwrap();
        assert_eq!(ttbb24.kind, "bundle");
        assert_eq!(ttbb24.total_cards, 2);
        assert!(ttbb24.base_total_cards.is_none());
    }

    #[test]
    fn analytics_returns_some_for_bundle() {
        let (_d, mut conn) = seed_bundle_ttbb24();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3-130-stamp_trick_or_trade".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let a = analytics(&conn, "ttbb-2024").unwrap().unwrap();
        assert_eq!(a.total_cards, 2);
        assert_eq!(a.owned_cards, 1);
        assert_eq!(a.copy_counts.len(), 2);
    }

    #[test]
    fn binder_returns_none_for_unknown_slug() {
        let (_d, conn) = seed_bundle_ttbb24();
        assert!(
            get_bundle_binder(&conn, "not-a-real-bundle", &BinderQuery::default())
                .unwrap()
                .is_none()
        );
    }
}
