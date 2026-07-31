//! Auto-discovery of brand-new sets from TCGCSV groups (pd-558b1e4f).
//!
//! `/browse` builds its tiles from the `sets` + `cards` catalog, which is
//! sourced from pokemontcg.io. That source runs weeks-to-months behind the
//! actual release and is flaky (a `/v2/sets` timeout failed a whole nightly
//! refresh once). TCGCSV, meanwhile, has the group — with products and
//! prices — the day the set lists. The gap is why "ME05: Pitch Black" sat
//! invisible after its 2026-07-17 release with all of its TCGCSV data
//! already imported.
//!
//! This module closes the gap automatically. After the TCGCSV import, every
//! group that bridged to no catalog set is checked against the eligibility
//! policy in `data/overrides/tcgcsv_set_discovery.json`; each survivor gets
//! a synthesized `sets` row (built from the group's own metadata) plus
//! synthesized `cards` rows (built from the group's products, via the same
//! builder the bridge overlay uses). The set is browseable that night.
//!
//! Three properties keep this safe to run unattended:
//!
//! * **Strictly additive.** A group is only considered when it links to no
//!   set, and a set is only created when its derived `set_code` is free.
//!   Discovery never re-points an existing bridge or overwrites a set row.
//! * **Superseded silently.** The derived `set_code` follows
//!   pokemontcg.io's own convention (`ME05` → `me5`, `SWSH01` → `swsh1`),
//!   so when upstream finally publishes the set, `upsert_set` lands on the
//!   same row and replaces the synthesized data in place. `import_tail`
//!   treats a set row with a NULL `ptcgio_fetched_at` as not-yet-imported
//!   precisely so that hand-off happens on the next refresh.
//! * **Visible provenance, with an escape hatch.** A discovered set carries
//!   `sets.discovered_from_group_id`, and the UI badges any set upstream
//!   hasn't confirmed. A mistaken tile is suppressed by adding its group to
//!   `deny_group_ids` in the policy file.
//!
//! The eligibility rule is the numbered era prefix — `ME05: Pitch Black`,
//! `SV10: Destined Rivals`, `SWSH12: Silver Tempest` — plus a floor on the
//! group's distinct collector numbers. It deliberately misses unnumbered
//! specials (`SV: Black Bolt`), energy umbrellas (`MEE: Mega Evolution
//! Energies`), promo catch-alls (`ME: Mega Evolution Promo`) and
//! collection groups (`First Partner Collection 2026`); those keep taking
//! the hand-authored `tcgcsv_set_bridges.json` route they already took.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;

use crate::error::Result;
use crate::tcgcsv::{normalize_collector_number, synthesize_cards_for_group};

const POLICY_JSON: &str = include_str!("../../../data/overrides/tcgcsv_set_discovery.json");

/// The eligibility policy, seeded from
/// `data/overrides/tcgcsv_set_discovery.json`.
#[derive(Debug, Clone, Deserialize)]
struct DiscoveryPolicy {
    /// A group needs at least this many distinct collector numbers among
    /// its single-card products before it can become a set.
    min_unique_card_numbers: usize,
    /// Groups that must never become binder tiles regardless of shape.
    deny_group_ids: Vec<DeniedGroup>,
    /// Era code (lowercase, e.g. `"me"`) → series name. Consulted before
    /// the sibling-group derivation; normally empty.
    series_by_era: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeniedGroup {
    group_id: i64,
    /// Why the group is denied. Documentation only — the JSON file is the
    /// documentation surface.
    #[serde(default)]
    #[allow(dead_code)]
    comment: Option<String>,
}

fn load_policy() -> Result<DiscoveryPolicy> {
    Ok(serde_json::from_str(POLICY_JSON)?)
}

/// One set created (or re-linked) by a discovery pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredSet {
    pub group_id: i64,
    pub set_code: String,
    pub name: String,
    pub series: String,
    /// `cards` rows freshly inserted for the set by this pass.
    pub cards: usize,
}

/// Split a numbered-expansion group name into its era code, sequence
/// number, and set name: `"ME05: Pitch Black"` → `("me", 5, "Pitch
/// Black")`.
///
/// The head before the first colon must be letters followed by digits and
/// nothing else — that shape is what separates a real numbered expansion
/// from the umbrella groups sharing the era (`"ME: Mega Evolution Promo"`,
/// `"MEE: Mega Evolution Energies"`, `"SV: Black Bolt"`) and from every
/// group with no prefix at all (`"Miscellaneous Cards & Products"`,
/// `"First Partner Collection 2026"`).
pub fn parse_numbered_group_name(raw: &str) -> Option<(String, u32, String)> {
    let (head, rest) = raw.split_once(':')?;
    let head = head.trim();
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    let digit_at = head.find(|c: char| c.is_ascii_digit())?;
    let (era, digits) = head.split_at(digit_at);
    if era.is_empty() || era.len() > 5 || !era.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    if digits.is_empty() || digits.len() > 2 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((
        era.to_ascii_lowercase(),
        digits.parse().ok()?,
        rest.to_string(),
    ))
}

/// The catalog `set_code` a numbered group resolves to. Matches
/// pokemontcg.io's convention — `("me", 5)` → `"me5"`, `("swsh", 1)` →
/// `"swsh1"` — so an upstream publish lands on the synthesized row instead
/// of duplicating it.
pub fn derive_set_code(era: &str, number: u32) -> String {
    format!("{era}{number}")
}

/// TCGCSV publishes `"2026-07-17T00:00:00"`; the `sets` table stores
/// pokemontcg.io's `"2026/07/17"`. Anything that isn't a leading ISO date
/// is dropped rather than stored in a second format.
fn release_date_from_published_on(published_on: Option<&str>) -> Option<String> {
    let date = published_on?.split('T').next()?;
    let mut parts = date.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || y.len() != 4
        || m.len() != 2
        || d.len() != 2
        || !date
            .chars()
            .filter(|c| *c != '-')
            .all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some(format!("{y}/{m}/{d}"))
}

/// Find the series a new set in an existing era belongs to, by reading it
/// off whichever already-linked group shares the era prefix — `"ME05"`
/// takes "Mega Evolution" from `"ME04: Chaos Rising"`. The highest-numbered
/// sibling wins (series names get rebranded mid-era far less often than
/// they get invented, and the newest sibling is the closest neighbour).
fn series_from_sibling_group(conn: &Connection, era: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare(
        "SELECT g.name, s.series \
           FROM tcgplayer_groups g \
           JOIN sets s ON s.set_code = g.set_code \
          WHERE g.set_code IS NOT NULL",
    )?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut best: Option<(u32, String)> = None;
    for (name, series) in rows {
        let Some((sibling_era, number, _)) = parse_numbered_group_name(&name) else {
            continue;
        };
        if sibling_era != era {
            continue;
        }
        if best.as_ref().is_none_or(|(n, _)| number > *n) {
            best = Some((number, series));
        }
    }
    Ok(best.map(|(_, series)| series))
}

/// How many distinct collector numbers a group's single-card products
/// cover. Normalized the same way the synthesized card rows are, so the
/// count is exactly the number of binder slots the set would get.
fn unique_card_numbers(conn: &Connection, group_id: i64) -> Result<usize> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT collector_number FROM tcgcsv_products \
          WHERE group_id = ?1 AND collector_number IS NOT NULL",
    )?;
    let numbers: Vec<String> = stmt
        .query_map([group_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(numbers
        .iter()
        .map(|n| normalize_collector_number(n))
        .collect::<std::collections::HashSet<_>>()
        .len())
}

/// Synthesize a set + cards for every TCGCSV group that looks like a real
/// numbered expansion the catalog doesn't have yet.
///
/// Runs after the TCGCSV product import (it reads `tcgcsv_products`) and
/// needs no network of its own. Idempotent: a second pass re-links and
/// re-heals the same rows without creating anything new.
pub fn discover_new_sets(conn: &mut Connection) -> Result<Vec<DiscoveredSet>> {
    let policy = load_policy()?;
    let denied: std::collections::HashSet<i64> =
        policy.deny_group_ids.iter().map(|d| d.group_id).collect();

    // Unlinked groups only — a group that bridges to a set, automatically
    // or via the overlay, is already someone else's.
    let candidates: Vec<(i64, String, Option<String>, Option<String>)> = {
        let mut stmt = conn.prepare(
            "SELECT group_id, name, abbreviation, published_on \
               FROM tcgplayer_groups \
              WHERE set_code IS NULL \
              ORDER BY group_id",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?
    };

    let mut discovered = Vec::new();
    for (group_id, group_name, abbreviation, published_on) in candidates {
        if denied.contains(&group_id) {
            continue;
        }
        let Some((era, number, set_name)) = parse_numbered_group_name(&group_name) else {
            continue;
        };
        if unique_card_numbers(conn, group_id)? < policy.min_unique_card_numbers {
            continue;
        }
        let set_code = derive_set_code(&era, number);

        // Who owns this set_code already? Only a row this same group
        // created earlier may be adopted — anything else (an upstream set,
        // a bridge-synthesized set, another group's discovery) keeps it.
        let owner: Option<Option<i64>> = conn
            .query_row(
                "SELECT discovered_from_group_id FROM sets WHERE set_code = ?1",
                [&set_code],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(owner) = owner
            && owner != Some(group_id)
        {
            continue;
        }
        let series = match policy.series_by_era.get(&era) {
            Some(s) => s.clone(),
            None => {
                series_from_sibling_group(conn, &era)?.unwrap_or_else(|| era.to_ascii_uppercase())
            }
        };
        let release_date = release_date_from_published_on(published_on.as_deref());
        let ptcgo_code = abbreviation.filter(|a| !a.is_empty());

        let tx = conn.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO sets \
               (set_code, ptcgo_code, name, series, release_date, \
                discovered_from_group_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                set_code,
                ptcgo_code,
                set_name,
                series,
                release_date,
                group_id
            ],
        )?;
        // Heal a row this discovery created on an earlier pass when the
        // group's metadata has since changed (TCGCSV corrects names and
        // fills in publishedOn after the fact). Scoped to rows upstream
        // hasn't stamped — once pokemontcg.io publishes the set, its data
        // is authoritative and discovery stops writing.
        tx.execute(
            "UPDATE sets \
                SET ptcgo_code   = ?2, \
                    name         = ?3, \
                    series       = ?4, \
                    release_date = ?5 \
              WHERE set_code = ?1 \
                AND discovered_from_group_id = ?6 \
                AND ptcgio_fetched_at IS NULL",
            rusqlite::params![
                set_code,
                ptcgo_code,
                set_name,
                series,
                release_date,
                group_id
            ],
        )?;
        tx.execute(
            "UPDATE tcgplayer_groups SET set_code = ?2, role = 'primary' \
              WHERE group_id = ?1",
            rusqlite::params![group_id, set_code],
        )?;
        let cards = synthesize_cards_for_group(&tx, group_id, &set_code, None)?;
        tx.commit()?;

        discovered.push(DiscoveredSet {
            group_id,
            set_code,
            name: set_name,
            series,
            cards,
        });
    }
    Ok(discovered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tcgcsv::{ExtendedDatum, TcgGroup, TcgProduct, import_groups, import_products};

    fn shared_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        (dir, conn)
    }

    /// `count` single-card products in `group_id`, numbered 1..=count.
    fn numbered_products(group_id: i64, first_product_id: i64, count: i64) -> Vec<TcgProduct> {
        (0..count)
            .map(|i| TcgProduct {
                product_id: first_product_id + i,
                group_id,
                name: format!("Card {} - {:03}", i + 1, i + 1),
                image_url: Some(format!("https://tcgplayer.example/{}.jpg", i + 1)),
                url: None,
                image_count: 1,
                extended_data: vec![
                    ExtendedDatum {
                        name: "Number".into(),
                        value: format!("{:03}/120", i + 1),
                    },
                    ExtendedDatum {
                        name: "Rarity".into(),
                        value: "Common".into(),
                    },
                ],
            })
            .collect()
    }

    #[test]
    fn parses_numbered_group_names_and_rejects_umbrellas() {
        assert_eq!(
            parse_numbered_group_name("ME05: Pitch Black"),
            Some(("me".into(), 5, "Pitch Black".into()))
        );
        assert_eq!(
            parse_numbered_group_name("SWSH01: Sword & Shield Base Set"),
            Some(("swsh".into(), 1, "Sword & Shield Base Set".into()))
        );
        assert_eq!(
            parse_numbered_group_name("SV10: Destined Rivals"),
            Some(("sv".into(), 10, "Destined Rivals".into()))
        );
        // Era umbrellas carry no number — they are promo/energy catch-alls,
        // never binder tiles.
        assert_eq!(parse_numbered_group_name("ME: Mega Evolution Promo"), None);
        assert_eq!(
            parse_numbered_group_name("MEE: Mega Evolution Energies"),
            None
        );
        // Unnumbered specials are real sets but not auto-discoverable —
        // they take the hand-authored bridge route.
        assert_eq!(parse_numbered_group_name("SV: Black Bolt"), None);
        // No prefix at all.
        assert_eq!(
            parse_numbered_group_name("Miscellaneous Cards & Products"),
            None
        );
        assert_eq!(
            parse_numbered_group_name("First Partner Collection 2026"),
            None
        );
        // Digits inside the name, not the prefix.
        assert_eq!(parse_numbered_group_name("McDonald's Promos 2024"), None);
    }

    #[test]
    fn derives_pokemontcg_style_set_codes() {
        assert_eq!(derive_set_code("me", 5), "me5");
        assert_eq!(derive_set_code("swsh", 1), "swsh1");
        assert_eq!(derive_set_code("sv", 10), "sv10");
    }

    #[test]
    fn converts_published_on_to_catalog_release_date() {
        assert_eq!(
            release_date_from_published_on(Some("2026-07-17T00:00:00")),
            Some("2026/07/17".into())
        );
        assert_eq!(release_date_from_published_on(None), None);
        assert_eq!(release_date_from_published_on(Some("")), None);
    }

    #[test]
    fn discovers_a_new_numbered_set_with_no_hand_authored_bridge() {
        // The pd-558b1e4f case: TCGCSV published group 24688 "ME05: Pitch
        // Black" on 2026-07-17 with a full product run; pokemontcg.io's
        // newest set is still ME04 Chaos Rising. Nothing in
        // tcgcsv_set_bridges.json mentions 24688 — discovery has to make
        // the set browseable on its own.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date, \
                               ptcgio_fetched_at) \
             VALUES ('me4','CRI','Chaos Rising','Mega Evolution','2026/05/22', \
                     '2026-05-23T00:00:00')",
            [],
        )
        .unwrap();
        let groups = vec![
            TcgGroup {
                group_id: 24655,
                name: "ME04: Chaos Rising".into(),
                abbreviation: Some("CRI".into()),
                published_on: Some("2026-05-22T00:00:00".into()),
            },
            TcgGroup {
                group_id: 24688,
                name: "ME05: Pitch Black".into(),
                abbreviation: Some("PBL".into()),
                published_on: Some("2026-07-17T00:00:00".into()),
            },
        ];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();
        import_products(
            &mut conn,
            &numbered_products(24688, 700_000, 120),
            "2026-07-31",
        )
        .unwrap();

        let found = discover_new_sets(&mut conn).unwrap();
        assert_eq!(found.len(), 1, "one new set discovered");
        assert_eq!(found[0].set_code, "me5");
        assert_eq!(found[0].cards, 120);

        let row: (String, Option<String>, String, Option<String>, Option<i64>) = conn
            .query_row(
                "SELECT name, ptcgo_code, series, release_date, discovered_from_group_id \
                   FROM sets WHERE set_code = 'me5'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                "Pitch Black".into(),
                Some("PBL".into()),
                // Series read off the linked ME04 sibling group.
                "Mega Evolution".into(),
                Some("2026/07/17".into()),
                Some(24688),
            )
        );

        // The group is bridged, so variant expansion and sealed products
        // find the set too.
        let bridged: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 24688",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bridged.as_deref(), Some("me5"));

        // Cards are browseable.
        let cards: i64 = conn
            .query_row(
                "SELECT count(*) FROM cards WHERE set_code = 'me5'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(cards, 120);

        // Idempotent: the group is bridged now, so a second pass has
        // nothing left to consider.
        assert_eq!(discover_new_sets(&mut conn).unwrap(), vec![]);

        // And if a later `import_groups` fails to re-establish the
        // auto-link (the set name is the only thing tying group to set
        // once the row exists), discovery re-adopts the row it created
        // rather than making a second one.
        conn.execute(
            "UPDATE tcgplayer_groups SET set_code = NULL WHERE group_id = 24688",
            [],
        )
        .unwrap();
        let readopted = discover_new_sets(&mut conn).unwrap();
        assert_eq!(readopted.len(), 1);
        assert_eq!(readopted[0].cards, 0, "no new card rows");
        let sets: i64 = conn
            .query_row(
                "SELECT count(*) FROM sets WHERE set_code = 'me5'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sets, 1);
    }

    #[test]
    fn ineligible_groups_never_become_binder_tiles() {
        // Everything discovery must leave alone: an energy umbrella, a
        // promo catch-all, a collection group, the MCAP catch-all, and a
        // numbered group whose product run is too thin to be a real set.
        let (_d, mut conn) = shared_db();
        let groups = vec![
            TcgGroup {
                group_id: 24461,
                name: "MEE: Mega Evolution Energies".into(),
                abbreviation: Some("MEE".into()),
                published_on: Some("2025-09-26T00:00:00".into()),
            },
            TcgGroup {
                group_id: 24722,
                name: "ME: 30th Celebration".into(),
                abbreviation: Some("30C".into()),
                published_on: Some("2026-09-16T00:00:00".into()),
            },
            TcgGroup {
                group_id: 24584,
                name: "First Partner Collection 2026".into(),
                abbreviation: None,
                published_on: Some("2026-03-30T00:00:00".into()),
            },
            TcgGroup {
                group_id: 2374,
                name: "ME99: Miscellaneous Cards & Products".into(),
                abbreviation: Some("MCAP".into()),
                published_on: Some("2026-07-30T00:00:00".into()),
            },
            TcgGroup {
                group_id: 24999,
                name: "ME06: Barely Listed".into(),
                abbreviation: Some("BRL".into()),
                published_on: Some("2026-09-01T00:00:00".into()),
            },
        ];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();
        // Each ineligible group is given plenty of products — only the
        // policy may keep them out, not an empty product table. MCAP is
        // named with a numbered prefix here on purpose: the denylist has
        // to hold even when the name rule would let it through.
        import_products(
            &mut conn,
            &numbered_products(24461, 100_000, 120),
            "2026-07-31",
        )
        .unwrap();
        import_products(
            &mut conn,
            &numbered_products(24722, 200_000, 120),
            "2026-07-31",
        )
        .unwrap();
        import_products(
            &mut conn,
            &numbered_products(24584, 300_000, 120),
            "2026-07-31",
        )
        .unwrap();
        import_products(
            &mut conn,
            &numbered_products(2374, 400_000, 180),
            "2026-07-31",
        )
        .unwrap();
        // Below the min_unique_card_numbers floor.
        import_products(
            &mut conn,
            &numbered_products(24999, 500_000, 12),
            "2026-07-31",
        )
        .unwrap();

        let found = discover_new_sets(&mut conn).unwrap();
        assert_eq!(found, vec![], "no ineligible group becomes a set");
        // (`import_groups` synthesizes MEP from the bridge overlay, so
        // count discovery's own rows rather than every set.)
        let sets: i64 = conn
            .query_row(
                "SELECT count(*) FROM sets WHERE discovered_from_group_id IS NOT NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(sets, 0);
        let linked: i64 = conn
            .query_row(
                "SELECT count(*) FROM tcgplayer_groups \
                  WHERE set_code IS NOT NULL AND group_id IN \
                        (24461, 24722, 24584, 2374, 24999)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(linked, 0);
    }

    #[test]
    fn never_hijacks_a_set_code_another_group_owns() {
        // The Trainer Gallery groups ("SWSH09: Brilliant Stars Trainer
        // Gallery") derive the same set_code as their parent expansion.
        // If one ever fails to auto-link, discovery must not adopt the
        // parent's swsh9 row — that would re-point a real set at the
        // wrong group's products.
        let (_d, mut conn) = shared_db();
        conn.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, release_date, \
                               ptcgio_fetched_at) \
             VALUES ('swsh9','BRS','Brilliant Stars','Sword & Shield','2022/02/25', \
                     '2026-05-23T00:00:00')",
            [],
        )
        .unwrap();
        let groups = vec![TcgGroup {
            group_id: 3020,
            name: "SWSH09: Brilliant Stars Trainer Gallery".into(),
            abbreviation: Some("SWSH09:TG".into()),
            published_on: Some("2022-02-25T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();
        import_products(
            &mut conn,
            &numbered_products(3020, 600_000, 120),
            "2026-07-31",
        )
        .unwrap();

        let found = discover_new_sets(&mut conn).unwrap();
        assert_eq!(found, vec![], "an owned set_code is left alone");
        let name: String = conn
            .query_row("SELECT name FROM sets WHERE set_code = 'swsh9'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Brilliant Stars");
        let bridged: Option<String> = conn
            .query_row(
                "SELECT set_code FROM tcgplayer_groups WHERE group_id = 3020",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(bridged, None);
    }

    #[test]
    fn upstream_publish_supersedes_the_discovered_set() {
        // Once pokemontcg.io publishes the real set, its upsert lands on
        // the discovered row (same set_code) and owns the metadata from
        // then on — discovery's heal must stand down.
        let (_d, mut conn) = shared_db();
        let groups = vec![TcgGroup {
            group_id: 24688,
            name: "ME05: Pitch Black".into(),
            abbreviation: Some("PBL".into()),
            published_on: Some("2026-07-17T00:00:00".into()),
        }];
        import_groups(&mut conn, &groups, "2026-07-31").unwrap();
        import_products(
            &mut conn,
            &numbered_products(24688, 700_000, 120),
            "2026-07-31",
        )
        .unwrap();
        discover_new_sets(&mut conn).unwrap();

        // pokemontcg.io lands the real set. `import_tail` reaches it
        // because the discovered row has a NULL `ptcgio_fetched_at`; the
        // upsert then replaces the synthesized metadata in place.
        let upstream = crate::pokemontcg::PokemonTcgSet {
            id: "me5".into(),
            name: "Pitch Black".into(),
            series: "Mega Evolution".into(),
            printed_total: Some(120),
            total: Some(142),
            ptcgo_code: Some("PBL".into()),
            release_date: Some("2026/07/17".into()),
            images: None,
        };
        crate::pokemon_tcg_data::upsert_set(&conn, &upstream, "2026-08-01T00:00:00").unwrap();

        // A later discovery pass must not overwrite upstream's row — even
        // when the group is unlinked again and TCGCSV has since renamed it.
        conn.execute(
            "UPDATE tcgplayer_groups \
                SET name = 'ME05: Renamed By TCGCSV', set_code = NULL \
              WHERE group_id = 24688",
            [],
        )
        .unwrap();
        discover_new_sets(&mut conn).unwrap();
        let (name, printed): (String, Option<i64>) = conn
            .query_row(
                "SELECT name, printed_total FROM sets WHERE set_code = 'me5'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(name, "Pitch Black", "upstream owns the set metadata");
        assert_eq!(printed, Some(120));
    }
}
