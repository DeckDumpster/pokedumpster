//! Binder-page assembly: a set rendered as ordered slots, one per card
//! number, with per-printing ownership and master-set progress (PLAN.md §6).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;

/// Catalog offsets baked into `number_sortable` (PLAN.md §3.4): subset
/// namespaces (Galarian/Trainer Gallery) start at 1000, promos at 9000.
const SUBSET_FLOOR: i64 = 1_000;
const PROMO_FLOOR: i64 = 9_000;

/// Set header shown above a binder.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BinderSetInfo {
    pub set_code: String,
    pub name: String,
    pub series: String,
    #[ts(type = "number | null")]
    pub total: Option<i64>,
    #[ts(type = "number | null")]
    pub printed_total: Option<i64>,
}

/// One printing within a slot.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SlotPrinting {
    pub printing_id: String,
    pub variant: String,
    pub deprecated: bool,
    #[ts(type = "number")]
    pub owned_count: i64,
    pub market_price: Option<f64>,
}

/// One binder slot — a single card number and all its printings.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BinderSlot {
    pub card_id: String,
    pub number: String,
    pub name: String,
    pub rarity: Option<String>,
    pub image_large: Option<String>,
    /// `base` | `secret` | `subset` | `promo`.
    pub section: String,
    pub printings: Vec<SlotPrinting>,
}

/// A single rendered binder page with master-set progress.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BinderPage {
    pub set: BinderSetInfo,
    #[ts(type = "number")]
    pub layout: i64,
    #[ts(type = "number")]
    pub page: i64,
    #[ts(type = "number")]
    pub total_pages: i64,
    /// Numbered-set card count, and how many of those the user owns.
    #[ts(type = "number")]
    pub base_total: i64,
    #[ts(type = "number")]
    pub base_owned: i64,
    /// Printing count across every visible section, and how many are owned.
    #[ts(type = "number")]
    pub master_total: i64,
    #[ts(type = "number")]
    pub master_owned: i64,
    pub slots: Vec<BinderSlot>,
}

/// A catalog card row read while assembling a binder.
struct CardRow {
    card_id: String,
    number: String,
    number_sortable: i64,
    name: String,
    rarity: Option<String>,
    image_large: Option<String>,
}

fn section_of(number_sortable: i64, printed_total: i64) -> &'static str {
    if number_sortable <= printed_total {
        "base"
    } else if number_sortable < SUBSET_FLOOR {
        "secret"
    } else if number_sortable < PROMO_FLOOR {
        "subset"
    } else {
        "promo"
    }
}

/// Assemble a binder page for `set_code`. `None` if the set is unknown.
pub fn get_binder_page(
    conn: &Connection,
    set_code: &str,
    page: i64,
    layout: i64,
    include_secret: bool,
    include_subset: bool,
    include_promos: bool,
) -> Result<Option<BinderPage>> {
    let set: Option<BinderSetInfo> = conn
        .prepare("SELECT set_code, name, series, total, printed_total FROM sets WHERE set_code = ?1")?
        .query_row([set_code], |r| {
            Ok(BinderSetInfo {
                set_code: r.get(0)?,
                name: r.get(1)?,
                series: r.get(2)?,
                total: r.get(3)?,
                printed_total: r.get(4)?,
            })
        })
        .optional()?;
    let Some(set) = set else {
        return Ok(None);
    };
    let printed_total = set.printed_total.unwrap_or(i64::MAX);

    // Every card in the set, in binder order.
    let cards: Vec<CardRow> = {
        let mut stmt = conn.prepare(
            "SELECT card_id, number, number_sortable, name, rarity, image_large \
             FROM cards WHERE set_code = ?1 ORDER BY number_sortable",
        )?;
        let rows = stmt.query_map([set_code], |r| {
            Ok(CardRow {
                card_id: r.get(0)?,
                number: r.get(1)?,
                number_sortable: r.get(2)?,
                name: r.get(3)?,
                rarity: r.get(4)?,
                image_large: r.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    // Every printing in the set, with owned counts and market price.
    let mut printings: HashMap<String, Vec<SlotPrinting>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT p.card_id, p.printing_id, p.variant, p.deprecated_at, \
                    (SELECT count(*) FROM collection c WHERE c.printing_id = p.printing_id), \
                    (SELECT lp.price FROM latest_prices lp \
                       WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id \
                         AND lp.price_type = 'market' LIMIT 1) \
             FROM printings p JOIN cards cd ON p.card_id = cd.card_id \
             WHERE cd.set_code = ?1 ORDER BY cd.number_sortable, p.variant",
        )?;
        let rows = stmt.query_map([set_code], |r| {
            let card_id: String = r.get(0)?;
            Ok((
                card_id,
                SlotPrinting {
                    printing_id: r.get(1)?,
                    variant: r.get(2)?,
                    deprecated: r.get::<_, Option<String>>(3)?.is_some(),
                    owned_count: r.get(4)?,
                    market_price: r.get(5)?,
                },
            ))
        })?;
        for row in rows {
            let (card_id, printing) = row?;
            printings.entry(card_id).or_default().push(printing);
        }
    }

    // Build every visible slot (all sections the include flags allow).
    let visible: Vec<BinderSlot> = cards
        .into_iter()
        .filter_map(|card| {
            let section = section_of(card.number_sortable, printed_total);
            let keep = match section {
                "secret" => include_secret,
                "subset" => include_subset,
                "promo" => include_promos,
                _ => true, // base
            };
            if !keep {
                return None;
            }
            let slot_printings = printings.get(&card.card_id).cloned().unwrap_or_default();
            Some(BinderSlot {
                card_id: card.card_id,
                number: card.number,
                name: card.name,
                rarity: card.rarity,
                image_large: card.image_large,
                section: section.to_string(),
                printings: slot_printings,
            })
        })
        .collect();

    // Master-set progress.
    let base_total = visible.iter().filter(|s| s.section == "base").count() as i64;
    let base_owned = visible
        .iter()
        .filter(|s| s.section == "base" && s.printings.iter().any(|p| p.owned_count > 0))
        .count() as i64;
    let master_total: i64 = visible.iter().map(|s| s.printings.len() as i64).sum();
    let master_owned = visible
        .iter()
        .flat_map(|s| &s.printings)
        .filter(|p| p.owned_count > 0)
        .count() as i64;

    // Paginate.
    let layout = layout.clamp(1, 60);
    let visible_count = visible.len() as i64;
    let total_pages = ((visible_count + layout - 1) / layout).max(1);
    let page = page.clamp(1, total_pages);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{self, NewCopy};
    use crate::{connect_user, open_shared};

    fn binder_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            // printed_total = 2 -> numbers 1,2 base; 3 secret; GG01 subset.
            c.execute(
                "INSERT INTO sets (set_code, name, series, total, printed_total) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet', 4, 2)",
                [],
            )
            .unwrap();
            let cards = [
                ("sv3pt5-1", "1", 1_i64),
                ("sv3pt5-2", "2", 2),
                ("sv3pt5-3", "3", 3),
                ("sv3pt5-GG01", "GG01", 1_001),
            ];
            for (id, num, ns) in cards {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, 'sv3pt5', ?2, ?3, 'Card')",
                    rusqlite::params![id, num, ns],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) VALUES (?1, ?2, 'normal')",
                    rusqlite::params![format!("{id}-normal"), id],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn assembles_sections_and_progress() {
        let (_d, mut conn) = binder_conn();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        // All sections on.
        let p = get_binder_page(&conn, "sv3pt5", 1, 9, true, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(p.slots.len(), 4);
        assert_eq!(p.base_total, 2);
        assert_eq!(p.base_owned, 1);
        assert_eq!(p.master_total, 4); // one printing per card
        assert_eq!(p.master_owned, 1);
        assert_eq!(p.slots[2].section, "secret");
        assert_eq!(p.slots[3].section, "subset");

        // Secret + subset off -> only the 2 base cards.
        let base_only = get_binder_page(&conn, "sv3pt5", 1, 9, false, false, false)
            .unwrap()
            .unwrap();
        assert_eq!(base_only.slots.len(), 2);
        assert!(base_only.slots.iter().all(|s| s.section == "base"));
    }

    #[test]
    fn paginates() {
        let (_d, conn) = binder_conn();
        let p1 = get_binder_page(&conn, "sv3pt5", 1, 2, true, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(p1.total_pages, 2);
        assert_eq!(p1.slots.len(), 2);
        assert_eq!(p1.slots[0].number, "1");

        let p2 = get_binder_page(&conn, "sv3pt5", 2, 2, true, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(p2.slots.len(), 2);
        assert_eq!(p2.slots[0].number, "3");

        // Out-of-range page clamps.
        let clamped = get_binder_page(&conn, "sv3pt5", 99, 2, true, true, true)
            .unwrap()
            .unwrap();
        assert_eq!(clamped.page, 2);
    }

    #[test]
    fn unknown_set_is_none() {
        let (_d, conn) = binder_conn();
        assert!(get_binder_page(&conn, "nope", 1, 9, true, true, true)
            .unwrap()
            .is_none());
    }
}
