//! TCGplayer mapping coverage — which sets came out of variant expansion
//! with no route to a price at all.
//!
//! A printing prices by joining `prices` on its `tcgplayer_product_id`, so
//! a printing without one can never price. One such printing is ordinary:
//! TCGplayer does not list every card. A whole *set* of them is not — it
//! means the set reached no TCGCSV group, and the only ways that happens
//! are a bridge nobody wrote and an auto-link that went to the wrong group
//! (`tcgcsv::resolve_group_set_links`).
//!
//! That failure used to be silent. `basep` (Wizards Black Star Promos)
//! sat at 0 of 53 printings mapped for the catalog's whole life, and what
//! surfaced it was not the catalog — it was the four manual price
//! overrides the collection needed to paper over it (pd-0o5m). This
//! module is the report that makes the next one loud: `pkdump setup` and
//! `pkdump data refresh`/`expand` print it after expansion, so a set that
//! maps to nothing says so on the run that broke it.
//!
//! The report is a statement of fact, not a failure. Some sets in it are
//! genuinely unlistable on TCGplayer; the point is that the list is short,
//! visible, and shrinks on purpose rather than by accident.

use rusqlite::Connection;

use crate::error::Result;

/// One set that produced printings and mapped none of them to a
/// TCGplayer product.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmappedSet {
    pub set_code: String,
    pub name: String,
    /// Live (non-deprecated) printings the set has.
    pub printings: i64,
    /// TCGCSV groups bridged to the set, if any. Empty means nothing
    /// bridged; non-empty means something did and still resolved nothing,
    /// which points at the group rather than at the bridge.
    pub group_ids: Vec<i64>,
}

/// Every set whose live printings all lack a `tcgplayer_product_id`,
/// widest first. Sets with no printings at all are not included — they
/// have nothing to map, and an empty set is a different bug.
pub fn unmapped_sets(conn: &Connection) -> Result<Vec<UnmappedSet>> {
    let mut stmt = conn.prepare(
        "SELECT s.set_code, s.name, COUNT(*) AS printings \
           FROM printings p \
           JOIN cards c ON c.card_id = p.card_id \
           JOIN sets  s ON s.set_code = c.set_code \
          WHERE p.deprecated_at IS NULL \
          GROUP BY s.set_code, s.name \
         HAVING COUNT(p.tcgplayer_product_id) = 0 \
          ORDER BY printings DESC, s.set_code",
    )?;
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut groups = conn.prepare("SELECT group_id FROM tcgplayer_groups WHERE set_code = ?1")?;
    let mut out = Vec::with_capacity(rows.len());
    for (set_code, name, printings) in rows {
        let group_ids: Vec<i64> = groups
            .query_map([&set_code], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        out.push(UnmappedSet {
            set_code,
            name,
            printings,
            group_ids,
        });
    }
    Ok(out)
}

/// Print the coverage report. Called at the end of every pipeline that
/// runs variant expansion.
pub fn report_unmapped_sets(conn: &Connection) -> Result<usize> {
    let unmapped = unmapped_sets(conn)?;
    if unmapped.is_empty() {
        println!("  every set maps at least one printing to a TCGplayer product");
        return Ok(0);
    }
    let printings: i64 = unmapped.iter().map(|u| u.printings).sum();
    println!(
        "  {} set(s), {printings} printing(s) map to NO TCGplayer product — these cannot price:",
        unmapped.len()
    );
    for u in &unmapped {
        let groups = if u.group_ids.is_empty() {
            "no bridged group".to_string()
        } else {
            format!(
                "group(s) {}",
                u.group_ids
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        println!(
            "    {:10} {:38} {:5} printings — {groups}",
            u.set_code, u.name, u.printings
        );
    }
    Ok(unmapped.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = pkdump_db::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        conn.execute_batch(
            "INSERT INTO sets (set_code, name, series) VALUES \
               ('good','Good Set','Test'), ('dark','Dark Set','Test'), \
               ('empty','Empty Set','Test');
             INSERT INTO cards (card_id, set_code, number, number_sortable, name, supertype) \
             VALUES \
               ('good-1','good','1',1,'Priced','Pokémon'), \
               ('dark-1','dark','1',1,'Unpriced One','Pokémon'), \
               ('dark-2','dark','2',2,'Unpriced Two','Pokémon');
             INSERT INTO tcgplayer_groups (group_id, set_code, name, fetched_at) \
             VALUES (77,'dark','Dark Group','2026-08-10');
             INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) VALUES \
               ('good-1-normal','good-1','normal',1001), \
               ('dark-1-normal','dark-1','normal',NULL), \
               ('dark-2-normal','dark-2','normal',NULL);",
        )
        .unwrap();
        (dir, conn)
    }

    #[test]
    fn reports_only_the_set_that_maps_nothing() {
        let (_d, conn) = catalog();
        let unmapped = unmapped_sets(&conn).unwrap();
        assert_eq!(
            unmapped,
            vec![UnmappedSet {
                set_code: "dark".into(),
                name: "Dark Set".into(),
                printings: 2,
                group_ids: vec![77],
            }],
            "a set with a mapped printing is fine, and a set with no printings \
             at all has nothing to map"
        );
    }

    #[test]
    fn a_deprecated_mapping_does_not_count_as_coverage() {
        // Soft-deprecation keeps the row (PLAN.md §4.4). A set whose only
        // product-bearing printing is deprecated maps nothing today.
        let (_d, conn) = catalog();
        conn.execute(
            "UPDATE printings SET deprecated_at = '2026-08-10' WHERE printing_id = 'good-1-normal'",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id) \
             VALUES ('good-1-holo','good-1','holo',NULL)",
            [],
        )
        .unwrap();

        let codes: Vec<String> = unmapped_sets(&conn)
            .unwrap()
            .into_iter()
            .map(|u| u.set_code)
            .collect();
        assert_eq!(codes, vec!["dark".to_string(), "good".to_string()]);
    }
}
