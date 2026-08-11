//! Binder-page assembly: a set rendered as ordered slots, one per card
//! number, with per-printing ownership and master-set progress (PLAN.md §6).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use crate::error::Result;

/// Catalog offsets baked into `number_sortable` (PLAN.md §3.4): subset
/// namespaces (Galarian/Trainer Gallery) start at 1000, promos at 9000.
const SUBSET_FLOOR: i64 = 1_000;
const PROMO_FLOOR: i64 = 9_000;

/// Header shown above a binder. Renders both real sets (kind="set") and
/// logical-set containers like Trick or Trade bundles (kind="bundle").
/// For bundles, `series` is the synthetic label ("Trick or Trade Bundle")
/// and total/printed_total are `None` so the base-completion bar hides.
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
    /// `"set"` for real sets, `"bundle"` for TTBB-style containers.
    pub kind: String,
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

/// Pointer to the home set of a slot's underlying card, when that set
/// differs from the container being rendered. Populated only for bundle
/// slots (TTBB products are reprints of cards in other sets) — `None`
/// for normal set slots.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ExternalSet {
    pub set_code: String,
    pub name: String,
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
    /// `base` | `secret` | `subset` | `promo`. Bundles use `base` for
    /// every slot since they have no rarity-tier sections.
    pub section: String,
    pub printings: Vec<SlotPrinting>,
    /// Home set of the slot's card when different from the container —
    /// bundle slots use this to surface "lives in Obsidian Flames" and
    /// to route the card-detail link. `None` for regular set slots.
    pub external_set: Option<ExternalSet>,
}

/// The set of cards a user is missing, ready for a TCGplayer Mass Entry
/// paste. Built by [`missing_for_export`]; the front-end filters by scope
/// (base vs master) and per-card secret-rare confirmation before assembling
/// the final paste text.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct MissingExport {
    pub set_code: String,
    pub set_name: String,
    /// The set code used in the Mass Entry lines: TCGplayer's own
    /// `group.abbreviation` when known, else the PTCGO code. `None` means the
    /// set has no usable code, so every line is unmappable (the UI warns).
    pub ptcgo_code: Option<String>,
    pub cards: Vec<MissingCard>,
}

/// One card the user owns no copy of, with its ready-to-paste Mass Entry
/// line (or `None` when the set lacks a `ptcgo_code` to anchor it).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct MissingCard {
    pub card_id: String,
    pub number: String,
    pub name: String,
    /// `base` | `secret` | `subset` | `promo`.
    pub section: String,
    /// `"1 <Name> - <###/###> [<CODE>]"` (or `"1 <Name> [<CODE>]"` when the
    /// card has no TCGplayer collector number), or `None` when unmappable
    /// (the set has no code).
    pub mass_entry_line: Option<String>,
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

/// How to sort, filter, and paginate a binder page.
#[derive(Debug, Clone)]
pub struct BinderQuery {
    pub page: i64,
    pub layout: i64,
    pub include_secret: bool,
    pub include_subset: bool,
    pub include_promos: bool,
    /// `number` (asc) | `number_desc` | `name` (A→Z) | `name_desc` |
    /// `price` (high→low) | `price_asc` | `rarity` (rare→common) |
    /// `rarity_asc`. Unknown values fall back to `number`.
    pub sort: String,
    /// Case-insensitive card-name substring. Empty means no search.
    pub search: String,
    /// `all` | `have` (own ≥1 printing) | `need` (own none) |
    /// `dupes` (own ≥2 of some printing).
    pub filter: String,
}

impl Default for BinderQuery {
    fn default() -> Self {
        Self {
            page: 1,
            layout: 9,
            include_secret: true,
            include_subset: true,
            include_promos: true,
            sort: "number".to_string(),
            search: String::new(),
            filter: "all".to_string(),
        }
    }
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

/// Build a card's TCGplayer Mass Entry line.
///
/// The reliable format for a card with multiple printings in one set (same
/// name, different numbers — e.g. a base copy plus its secret-rare reprint)
/// reproduces TCGplayer's *own product name*, which embeds the padded
/// collector fraction, and appends the set code in brackets:
///
/// ```text
/// 1 Mega Zeraora ex - 027/084 [PBL]
///   {name} - {collector_number} [{set_code}]
/// ```
///
/// The number must be the padded `###/###` fraction *with the set total*
/// (`027/084`, `101/084` for a secret rare) — that is what disambiguates
/// the multiple printings. Our previous line put the set code before a
/// trailing number (`... [PBL] 027/084`); that parses, but TCGplayer fails
/// to resolve multi-printing cards from it, hence "could not be found".
///
/// When the card has no linked TCGplayer product (no collector number),
/// drop the number entirely (`1 {name} [{code}]`) — valid for the
/// single-printing case, which is the only one that lacks a number anyway.
fn mass_entry_line(name: &str, code: &str, collector_number: Option<&str>) -> String {
    match collector_number {
        Some(num) => format!("1 {name} - {num} [{code}]"),
        None => format!("1 {name} [{code}]"),
    }
}

/// Rank a Pokémon rarity low (common) → high (chase). Unrecognised
/// rarities sort just above the staple rares so they stay visible.
fn rarity_rank(rarity: &Option<String>) -> i64 {
    let Some(r) = rarity else { return 0 };
    match r.to_ascii_lowercase().as_str() {
        "common" => 1,
        "uncommon" => 2,
        "rare" => 3,
        "promo" | "classic collection" => 4,
        "rare holo" => 5,
        "radiant rare" => 6,
        "rare holo ex" | "rare holo gx" | "rare holo v" | "double rare" => 7,
        "rare holo vmax" | "rare holo vstar" | "ultra rare" => 8,
        "amazing rare" | "rare shiny" | "rare shiny gx" => 9,
        "illustration rare" => 10,
        "trainer gallery rare holo" => 11,
        "rare secret" | "rare rainbow" => 12,
        "special illustration rare" => 13,
        "hyper rare" | "rare holo star" => 14,
        _ => 6,
    }
}

/// A slot's price for sorting — the dearest market price across its
/// printings; slots with no priced printing sort as 0.
fn slot_price(slot: &BinderSlot) -> f64 {
    slot.printings
        .iter()
        .filter_map(|p| p.market_price)
        .fold(0.0_f64, f64::max)
}

/// Assemble a binder page for `set_code`. `None` if the set is unknown.
pub fn get_binder_page(
    conn: &Connection,
    set_code: &str,
    q: &BinderQuery,
) -> Result<Option<BinderPage>> {
    let set: Option<BinderSetInfo> = conn
        .prepare(
            "SELECT set_code, name, series, total, printed_total FROM sets WHERE set_code = ?1",
        )?
        .query_row([set_code], |r| {
            Ok(BinderSetInfo {
                set_code: r.get(0)?,
                name: r.get(1)?,
                series: r.get(2)?,
                total: r.get(3)?,
                printed_total: r.get(4)?,
                kind: "set".to_string(),
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
        let mut stmt = conn.prepare(concat!(
            "SELECT p.card_id, p.printing_id, p.variant, p.deprecated_at, \
                    (SELECT count(*) FROM collection c WHERE c.printing_id = p.printing_id), \
                    ",
            crate::market_price_expr!(),
            " \
             FROM ( \
                SELECT card_id, printing_id, variant, deprecated_at, \
                       tcgplayer_product_id, sub_type_name FROM printings \
                UNION ALL \
                SELECT card_id, printing_id, variant, NULL AS deprecated_at, \
                       NULL AS tcgplayer_product_id, NULL AS sub_type_name \
                  FROM user_printings \
             ) p \
             JOIN cards cd ON p.card_id = cd.card_id \
             WHERE cd.set_code = ?1 ORDER BY cd.number_sortable, p.variant",
        ))?;
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
    let mut visible: Vec<BinderSlot> = cards
        .into_iter()
        .filter_map(|card| {
            let section = section_of(card.number_sortable, printed_total);
            let keep = match section {
                "secret" => q.include_secret,
                "subset" => q.include_subset,
                "promo" => q.include_promos,
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
                external_set: None,
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

    // Sort. Progress above is computed pre-sort/filter, so the bars always
    // reflect the whole section-included set rather than the current view.
    // `visible` arrives in collector-number order from the catalog query.
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
        "rarity" => visible.sort_by(|a, b| rarity_rank(&b.rarity).cmp(&rarity_rank(&a.rarity))),
        "rarity_asc" => visible.sort_by(|a, b| rarity_rank(&a.rarity).cmp(&rarity_rank(&b.rarity))),
        _ => {} // "number" — already collector-number ascending.
    }

    // Filter: in-set name search, then the ownership tab.
    let search = q.search.trim().to_ascii_lowercase();
    let owns_one = |s: &BinderSlot| s.printings.iter().any(|p| p.owned_count > 0);
    visible.retain(|s| {
        if !search.is_empty() && !s.name.to_ascii_lowercase().contains(&search) {
            return false;
        }
        match q.filter.as_str() {
            "have" => owns_one(s),
            "need" => !owns_one(s),
            "dupes" => s.printings.iter().any(|p| p.owned_count >= 2),
            _ => true, // "all"
        }
    });

    // Paginate the sorted, filtered view.
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

/// Every card in `set_code` the user owns zero copies of, with a TCGplayer
/// Mass Entry line per card. `None` if the set is unknown.
///
/// "Missing" mirrors the binder's `need` filter: a card counts as owned if
/// any of its printings — catalog or the user-printings escape hatch — has a
/// collection row. One line per card (the user picks holo/normal in
/// TCGplayer's cart review), so variants are intentionally collapsed.
pub fn missing_for_export(conn: &Connection, set_code: &str) -> Result<Option<MissingExport>> {
    struct SetInfo {
        name: String,
        ptcgo_code: Option<String>,
        printed_total: Option<i64>,
        tcg_abbrev: Option<String>,
    }
    let set: Option<SetInfo> = conn
        .prepare(
            "SELECT s.name, s.ptcgo_code, s.printed_total, \
                    (SELECT g.abbreviation FROM tcgplayer_groups g \
                      WHERE g.set_code = s.set_code AND g.abbreviation IS NOT NULL \
                      ORDER BY (g.role = 'primary') DESC LIMIT 1) \
             FROM sets s WHERE s.set_code = ?1",
        )?
        .query_row([set_code], |r| {
            Ok(SetInfo {
                name: r.get(0)?,
                ptcgo_code: r.get(1)?,
                printed_total: r.get(2)?,
                tcg_abbrev: r.get(3)?,
            })
        })
        .optional()?;
    let Some(SetInfo {
        name: set_name,
        ptcgo_code,
        printed_total,
        tcg_abbrev,
    }) = set
    else {
        return Ok(None);
    };
    let printed_total = printed_total.unwrap_or(i64::MAX);
    // Mass Entry is a TCGplayer feature, so the bracketed set code must be
    // TCGplayer's own abbreviation (from the ingested tcgplayer_groups), not
    // the Pokémon-TCGO code — they only sometimes coincide. Fall back to the
    // PTCGO code when TCGplayer has no abbreviation.
    let entry_set_code = tcg_abbrev.or(ptcgo_code);

    let mut stmt = conn.prepare(
        "SELECT cd.card_id, cd.number, cd.number_sortable, cd.name, \
                (SELECT tp.collector_number FROM printings p \
                   JOIN tcgcsv_products tp ON tp.product_id = p.tcgplayer_product_id \
                  WHERE p.card_id = cd.card_id AND p.tcgplayer_product_id IS NOT NULL \
                    AND tp.collector_number IS NOT NULL LIMIT 1) AS tcg_number \
         FROM cards cd \
         WHERE cd.set_code = ?1 \
           AND NOT EXISTS ( \
             SELECT 1 FROM ( \
                SELECT printing_id FROM printings WHERE card_id = cd.card_id \
                UNION ALL \
                SELECT printing_id FROM user_printings WHERE card_id = cd.card_id \
             ) p \
             JOIN collection c ON c.printing_id = p.printing_id \
           ) \
         ORDER BY cd.number_sortable",
    )?;
    let cards = stmt
        .query_map([set_code], |r| {
            let card_id: String = r.get(0)?;
            let number: String = r.get(1)?;
            let number_sortable: i64 = r.get(2)?;
            let name: String = r.get(3)?;
            let tcg_number: Option<String> = r.get(4)?;
            let section = section_of(number_sortable, printed_total).to_string();
            // Reproduce TCGplayer's own product name ("Name - 027/084")
            // followed by [SET]; the padded collector fraction is what lets
            // Mass Entry resolve cards that have several printings in the set.
            let line = entry_set_code
                .as_ref()
                .map(|code| mass_entry_line(&name, code, tcg_number.as_deref()));
            Ok(MissingCard {
                card_id,
                number,
                name,
                section,
                mass_entry_line: line,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    // Expose the code actually used in the lines (TCGplayer abbrev, or the
    // PTCGO fallback) so the UI's "no code → unmappable" warning stays honest.
    Ok(Some(MissingExport {
        set_code: set_code.to_string(),
        set_name,
        ptcgo_code: entry_set_code,
        cards,
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

    // Regression for pokedumpster-3s2: a card owned only
    // through a custom variant (user_printings — the "Missing Variant" escape
    // hatch) must still highlight as owned in the binder, count toward
    // base_owned, and surface the custom variant as its own slot printing. The
    // binder query was catalog-only (printings, no user_printings), so the slot
    // stayed unowned. User-reported on card/base2/9.
    #[test]
    fn slot_owns_card_via_custom_variant() {
        use crate::user_printings::{self, NewUserPrinting};
        let (_d, mut conn) = binder_conn();

        // A custom variant on card #2, and one owned copy of it. Card #2 has no
        // owned catalog printing.
        let up = user_printings::insert(
            &conn,
            &NewUserPrinting {
                card_id: "sv3pt5-2".into(),
                description: Some("inverted holo misprint".into()),
            },
        )
        .unwrap();
        assert_eq!(up.variant, "missing_variant");
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: up.printing_id.clone(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let p = get_binder_page(&conn, "sv3pt5", &BinderQuery::default())
            .unwrap()
            .unwrap();
        let slot = p
            .slots
            .iter()
            .find(|s| s.card_id == "sv3pt5-2")
            .expect("card #2 slot present");
        let custom = slot
            .printings
            .iter()
            .find(|pr| pr.printing_id == up.printing_id)
            .expect("custom variant surfaces as a slot printing");
        assert_eq!(custom.variant, "missing_variant");
        assert_eq!(custom.owned_count, 1, "the custom-variant copy is counted");
        assert!(
            slot.printings.iter().any(|pr| pr.owned_count > 0),
            "slot reads as owned"
        );
        // base_owned counts the card even though only its custom variant is owned.
        assert_eq!(p.base_owned, 1);
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
        let p = get_binder_page(&conn, "sv3pt5", &BinderQuery::default())
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
        let base_only = get_binder_page(
            &conn,
            "sv3pt5",
            &BinderQuery {
                include_secret: false,
                include_subset: false,
                include_promos: false,
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(base_only.slots.len(), 2);
        assert!(base_only.slots.iter().all(|s| s.section == "base"));
    }

    #[test]
    fn paginates() {
        let (_d, conn) = binder_conn();
        let q = |page: i64| BinderQuery {
            page,
            layout: 2,
            ..Default::default()
        };
        let p1 = get_binder_page(&conn, "sv3pt5", &q(1)).unwrap().unwrap();
        assert_eq!(p1.total_pages, 2);
        assert_eq!(p1.slots.len(), 2);
        assert_eq!(p1.slots[0].number, "1");

        let p2 = get_binder_page(&conn, "sv3pt5", &q(2)).unwrap().unwrap();
        assert_eq!(p2.slots.len(), 2);
        assert_eq!(p2.slots[0].number, "3");

        // Out-of-range page clamps.
        let clamped = get_binder_page(&conn, "sv3pt5", &q(99)).unwrap().unwrap();
        assert_eq!(clamped.page, 2);
    }

    #[test]
    fn sorts_searches_and_filters() {
        let (_d, mut conn) = binder_conn();
        // Own card #1 twice (a duplicate) and #2 once.
        for pid in ["sv3pt5-1-normal", "sv3pt5-1-normal", "sv3pt5-2-normal"] {
            collection::add(
                &mut conn,
                &NewCopy {
                    printing_id: pid.into(),
                    source: "manual_id".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let q = |sort: &str, filter: &str, search: &str| BinderQuery {
            sort: sort.into(),
            filter: filter.into(),
            search: search.into(),
            ..Default::default()
        };

        // number_desc reverses collector order: promo/subset last → first.
        let desc = get_binder_page(&conn, "sv3pt5", &q("number_desc", "all", ""))
            .unwrap()
            .unwrap();
        assert_eq!(desc.slots[0].number, "GG01");
        assert_eq!(desc.slots[3].number, "1");

        // The reverse-direction siblings round-trip — name_desc, price_asc,
        // rarity_asc just flip the comparator. Smoke-test that they parse
        // and produce a page (full ordering is exercised by the desc
        // variants above).
        for s in [
            "name",
            "name_desc",
            "price",
            "price_asc",
            "rarity",
            "rarity_asc",
        ] {
            let page = get_binder_page(&conn, "sv3pt5", &q(s, "all", "")).unwrap();
            assert!(page.is_some(), "sort '{s}' should produce a page");
        }

        // "need" → cards with no owned printing (#3 and GG01).
        let need = get_binder_page(&conn, "sv3pt5", &q("number", "need", ""))
            .unwrap()
            .unwrap();
        assert_eq!(need.slots.len(), 2);
        assert!(
            need.slots
                .iter()
                .all(|s| s.number == "3" || s.number == "GG01")
        );

        // "have" → #1 and #2; progress still reflects the whole set.
        let have = get_binder_page(&conn, "sv3pt5", &q("number", "have", ""))
            .unwrap()
            .unwrap();
        assert_eq!(have.slots.len(), 2);
        assert_eq!(have.base_total, 2, "progress is pre-filter");

        // "dupes" → only #1 (owned twice).
        let dupes = get_binder_page(&conn, "sv3pt5", &q("number", "dupes", ""))
            .unwrap()
            .unwrap();
        assert_eq!(dupes.slots.len(), 1);
        assert_eq!(dupes.slots[0].number, "1");

        // Search narrows by card name (every fixture card is named "Card").
        let hit = get_binder_page(&conn, "sv3pt5", &q("number", "all", "car"))
            .unwrap()
            .unwrap();
        assert_eq!(hit.slots.len(), 4);
        let miss = get_binder_page(&conn, "sv3pt5", &q("number", "all", "zzz"))
            .unwrap()
            .unwrap();
        assert!(miss.slots.is_empty());
    }

    #[test]
    fn missing_export_lines_and_sections() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            // 'tst' has a ptcgo_code (mappable); 'noab' has none (unmappable).
            c.execute(
                "INSERT INTO sets (set_code, name, series, ptcgo_code, printed_total) \
                 VALUES ('tst', 'Test Set', 'S&V', 'TST', 2)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, printed_total) \
                 VALUES ('noab', 'No Abbr', 'S&V', 1)",
                [],
            )
            .unwrap();
            // tst: base 1,2; secret 3; subset GG01.
            let cards = [
                ("tst", "tst-1", "1", 1_i64, "Bulbasaur"),
                ("tst", "tst-2", "2", 2, "Ivysaur"),
                ("tst", "tst-3", "3", 3, "Venusaur"),
                ("tst", "tst-GG01", "GG01", 1_001, "Pikachu"),
                ("noab", "noab-1", "1", 1, "Mew"),
            ];
            for (set, id, num, ns, name) in cards {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, set, num, ns, name],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) VALUES (?1, ?2, 'normal')",
                    rusqlite::params![format!("{id}-normal"), id],
                )
                .unwrap();
            }
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();

        // Own card #1 of tst — it must drop out of the missing list.
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "tst-1-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let exp = missing_for_export(&conn, "tst").unwrap().unwrap();
        assert_eq!(exp.ptcgo_code.as_deref(), Some("TST"));
        let nums: Vec<&str> = exp.cards.iter().map(|c| c.number.as_str()).collect();
        assert_eq!(nums, ["2", "3", "GG01"], "owned #1 excluded, rest ordered");
        let venusaur = exp.cards.iter().find(|c| c.number == "3").unwrap();
        assert_eq!(venusaur.section, "secret");
        // No linked TCGplayer product → number-less fallback line.
        assert_eq!(
            venusaur.mass_entry_line.as_deref(),
            Some("1 Venusaur [TST]")
        );
        assert_eq!(
            exp.cards
                .iter()
                .find(|c| c.number == "GG01")
                .unwrap()
                .section,
            "subset"
        );

        // No ptcgo_code → every line is None so the UI can warn.
        let noab = missing_for_export(&conn, "noab").unwrap().unwrap();
        assert!(noab.ptcgo_code.is_none());
        assert_eq!(noab.cards.len(), 1);
        assert!(noab.cards[0].mass_entry_line.is_none());
    }

    #[test]
    fn mass_entry_line_shapes() {
        // Product-name form (with collector fraction) then [SET].
        assert_eq!(
            mass_entry_line("Mega Zeraora ex", "PBL", Some("027/084")),
            "1 Mega Zeraora ex - 027/084 [PBL]"
        );
        // No collector number → number-less form (single-printing case).
        assert_eq!(mass_entry_line("Pineco", "SV01", None), "1 Pineco [SV01]");
    }

    #[test]
    fn mass_entry_uses_tcgplayer_abbreviation_and_product_name_form() {
        // TCGplayer needs its OWN set abbreviation ("MEG", not the PTCGO code)
        // and its product-name form ("Vulpix - 138/132 [MEG]"), NOT the set
        // code before a trailing number — the reported "could not be found".
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, ptcgo_code, printed_total) \
                 VALUES ('me1', 'Mega Evolution', 'ME', 'MEG', 132)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO tcgplayer_groups (group_id, set_code, name, abbreviation, fetched_at, role) \
                 VALUES (24380, 'me1', 'ME01: Mega Evolution', 'MEG', '2026-01-01', 'primary')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('me1-138', 'me1', '138', 138, 'Vulpix')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO tcgcsv_products (product_id, group_id, name, collector_number, fetched_at) \
                 VALUES (555, 24380, 'Vulpix - 138/132', '138/132', '2026-01-01')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) \
                 VALUES ('me1-138-holo', 'me1-138', 'holo', 555)",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let exp = missing_for_export(&conn, "me1").unwrap().unwrap();
        let vulpix = exp.cards.iter().find(|c| c.number == "138").unwrap();
        assert_eq!(
            vulpix.mass_entry_line.as_deref(),
            Some("1 Vulpix - 138/132 [MEG]")
        );
    }

    #[test]
    fn mass_entry_secret_rare_uses_padded_fraction_and_name_form() {
        // Regression for the PBL "could not be found" report: a secret rare
        // must render in TCGplayer's product-name form with the padded
        // fraction — "1 Fomantis - 085/084 [PBL]", NOT "[PBL] 085/084".
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, ptcgo_code, printed_total) \
                 VALUES ('pbl', 'Pitch Black', 'ME', 'PBL', 84)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO tcgplayer_groups (group_id, set_code, name, abbreviation, fetched_at, role) \
                 VALUES (25000, 'pbl', 'PBL: Pitch Black', 'PBL', '2026-01-01', 'primary')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('pbl-85', 'pbl', '85', 85, 'Fomantis')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO tcgcsv_products (product_id, group_id, name, collector_number, fetched_at) \
                 VALUES (777, 25000, 'Fomantis - 085/084', '085/084', '2026-01-01')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) \
                 VALUES ('pbl-85-holo', 'pbl-85', 'holo', 777)",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let exp = missing_for_export(&conn, "pbl").unwrap().unwrap();
        let fomantis = exp.cards.iter().find(|c| c.number == "85").unwrap();
        assert_eq!(
            fomantis.mass_entry_line.as_deref(),
            Some("1 Fomantis - 085/084 [PBL]")
        );
    }

    #[test]
    fn unknown_set_is_none() {
        let (_d, conn) = binder_conn();
        assert!(
            get_binder_page(&conn, "nope", &BinderQuery::default())
                .unwrap()
                .is_none()
        );
        assert!(missing_for_export(&conn, "nope").unwrap().is_none());
    }
}
