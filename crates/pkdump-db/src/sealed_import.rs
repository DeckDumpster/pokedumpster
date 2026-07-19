//! Sealed-product import (PLAN.md §9, sealed half of the garden wall).
//!
//! The singles importer ([`crate::import`]) resolves [`ParsedRow`]s against
//! the card catalog and writes to `collection`. This is its deliberate
//! mirror for sealed products: it resolves [`ParsedSealedRow`]s against the
//! sealed-product catalog and writes to `sealed_collection`. The two never
//! share a destination table, a resolver, or a preview — that separation is
//! the point.
//!
//! Unlike singles, sealed items keep a `quantity` (the collection table
//! aggregates by count), so rows are *not* expanded 1:1.
//!
//! [`ParsedRow`]: pkdump_core::import::ParsedRow

use std::collections::HashMap;

use rusqlite::Connection;

use pkdump_core::import::ParsedSealedRow;

use crate::error::Result;
use crate::import::resolve_set;
use crate::sealed::{self, NewSealed};

/// A sealed row that resolved cleanly to a catalog product — ready to commit.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ResolvedSealedRow {
    #[ts(type = "number")]
    pub source_line: u32,
    #[ts(type = "number")]
    pub product_id: i64,
    pub name: String,
    pub category: String,
    pub set_code: Option<String>,
    #[ts(type = "number")]
    pub quantity: u32,
    pub condition: String,
    pub purchase_price: Option<f64>,
    pub purchase_date: Option<String>,
    pub notes: Option<String>,
}

/// A sealed row that could not be resolved, with a human-readable reason.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct UnmatchedSealedRow {
    #[ts(type = "number")]
    pub source_line: u32,
    pub name: String,
    pub set_hint: String,
    pub reason: String,
}

/// The outcome of resolving the sealed rows of an import file — the sealed
/// half of the preview shown before commit.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SealedResolutionReport {
    pub matched: Vec<ResolvedSealedRow>,
    pub unmatched: Vec<UnmatchedSealedRow>,
}

/// The outcome of committing the sealed half of an import.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SealedCommitResult {
    #[ts(type = "number")]
    pub added: u32,
    #[ts(type = "number")]
    pub skipped: u32,
}

/// One catalog product, as loaded for name-matching within a set.
struct Candidate {
    product_id: i64,
    name: String,
    category: String,
    set_code: Option<String>,
}

/// Normalize a product name for tolerant matching across platforms: fold
/// case, straighten curly apostrophes, drop the accent on `é` (Pokémon),
/// fold hyphens/dashes to spaces (catalog "Super-Premium" vs Collectr
/// "Super Premium"), and collapse runs of whitespace. Mirrors the spirit
/// of the shared column-value mappings in `pkdump-core::import`.
///
/// Shared with the singles resolver ([`crate::import`]) for its global
/// name fallback.
pub(crate) fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_ws = false;
    for c in name.chars() {
        let c = match c {
            '\u{2019}' | '\u{2018}' | '`' | '\u{02bc}' => '\'',
            'é' | 'É' | 'è' | 'ë' => 'e',
            // Fold hyphens and dashes to spaces so they collapse like any
            // other separator.
            '-' | '\u{2010}' | '\u{2011}' | '\u{2013}' | '\u{2014}' => ' ',
            other => other,
        };
        if c.is_whitespace() {
            if !prev_ws && !out.is_empty() {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.extend(c.to_lowercase());
            prev_ws = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Load every sealed catalog product in a set, for name-matching. Products
/// without a `set_code` are only reachable when `set_code` is `None`.
fn candidates_in_set(conn: &Connection, set_code: &str) -> Result<Vec<Candidate>> {
    let mut stmt = conn.prepare(
        "SELECT product_id, name, category, set_code \
         FROM sealed_products WHERE set_code = ?1 COLLATE NOCASE",
    )?;
    let rows = stmt.query_map([set_code], |r| {
        Ok(Candidate {
            product_id: r.get(0)?,
            name: r.get(1)?,
            category: r.get(2)?,
            set_code: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Load every sealed catalog product — across all sets, and including the
/// many products with a NULL `set_code` (UPCs, Tin Displays). The candidate
/// pool for the global name fallback when set resolution fails or a product
/// isn't found in its stated set (pokedumpster-oq3i.3).
fn all_candidates(conn: &Connection) -> Result<Vec<Candidate>> {
    let mut stmt =
        conn.prepare("SELECT product_id, name, category, set_code FROM sealed_products")?;
    let rows = stmt.query_map([], |r| {
        Ok(Candidate {
            product_id: r.get(0)?,
            name: r.get(1)?,
            category: r.get(2)?,
            set_code: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The result of matching one parsed name against a set's candidates.
enum Match<'a> {
    One(&'a Candidate),
    /// More than one plausible product — the user must disambiguate.
    Ambiguous(Vec<&'a str>),
    None,
}

/// Match a parsed product name against a set's catalog candidates: exact
/// normalized name first, then a unique substring match (either direction).
fn match_name<'a>(name: &str, candidates: &'a [Candidate]) -> Match<'a> {
    let want = normalize_name(name);

    let exact: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| normalize_name(&c.name) == want)
        .collect();
    match exact.as_slice() {
        [one] => return Match::One(one),
        [] => {}
        many => return Match::Ambiguous(many.iter().map(|c| c.name.as_str()).collect()),
    }

    let subset: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            let have = normalize_name(&c.name);
            have.contains(&want) || want.contains(&have)
        })
        .collect();
    match subset.as_slice() {
        [one] => Match::One(one),
        [] => Match::None,
        many => Match::Ambiguous(many.iter().map(|c| c.name.as_str()).collect()),
    }
}

/// Resolve parsed sealed rows against the catalog, partitioning them into
/// matched products and unmatched rows with reasons.
pub fn resolve_sealed(
    conn: &Connection,
    rows: &[ParsedSealedRow],
) -> Result<SealedResolutionReport> {
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    // An import usually touches a handful of sets — cache set resolution and
    // each set's candidate list.
    let mut set_cache: HashMap<(String, Option<String>), Option<String>> = HashMap::new();
    let mut cand_cache: HashMap<String, Vec<Candidate>> = HashMap::new();
    // The global candidate pool (all sets, incl. NULL set_code) is loaded
    // lazily the first time a row needs the fallback.
    let mut all_cache: Option<Vec<Candidate>> = None;

    for row in rows {
        let line = row.source_line as u32;
        let miss = |reason: String| UnmatchedSealedRow {
            source_line: line,
            name: row.name.clone(),
            set_hint: row.set_hint.clone(),
            reason,
        };
        let resolved = |c: &Candidate| ResolvedSealedRow {
            source_line: line,
            product_id: c.product_id,
            name: c.name.clone(),
            category: c.category.clone(),
            set_code: c.set_code.clone(),
            quantity: row.quantity,
            condition: row.condition.clone(),
            purchase_price: row.purchase_price,
            purchase_date: row.purchase_date.clone(),
            notes: row.notes.clone(),
        };

        let key = (row.set_hint.clone(), row.set_name.clone());
        let set_code = match set_cache.get(&key) {
            Some(c) => c.clone(),
            None => {
                let c = resolve_set(conn, &row.set_hint, row.set_name.as_deref())?;
                set_cache.insert(key, c.clone());
                c
            }
        };

        // First, try to match within the stated set (when it resolved). A
        // hit or a genuine in-set ambiguity short-circuits; only an outright
        // miss falls through to the global search.
        if let Some(sc) = &set_code {
            if !cand_cache.contains_key(sc) {
                let c = candidates_in_set(conn, sc)?;
                cand_cache.insert(sc.clone(), c);
            }
            match match_name(&row.name, &cand_cache[sc]) {
                Match::One(c) => {
                    matched.push(resolved(c));
                    continue;
                }
                Match::Ambiguous(names) => {
                    unmatched.push(miss(format!(
                        "ambiguous in {sc}: matches {}",
                        names.join(", ")
                    )));
                    continue;
                }
                Match::None => {}
            }
        }

        // Fallback: the stated set didn't resolve, or the product wasn't in
        // it. Search the whole sealed catalog, NULL-set-code products
        // included (UPCs, Tin Displays, cross-set promos).
        if all_cache.is_none() {
            all_cache = Some(all_candidates(conn)?);
        }
        let all = all_cache.as_ref().unwrap();
        match match_name(&row.name, all) {
            Match::One(c) => matched.push(resolved(c)),
            Match::Ambiguous(names) => unmatched.push(miss(format!(
                "ambiguous across all sets: matches {}",
                names.join(", ")
            ))),
            Match::None => unmatched.push(miss(match &set_code {
                Some(sc) => format!("no sealed product '{}' in {sc} or any set", row.name),
                None => format!(
                    "no sealed product '{}' in any set (set '{}' unknown)",
                    row.name, row.set_hint
                ),
            })),
        }
    }
    Ok(SealedResolutionReport { matched, unmatched })
}

/// Commit the matched rows of a sealed resolution: insert each into the
/// user's sealed collection under the given `source`. One `sealed_collection`
/// row per matched line (quantity preserved, never expanded).
pub fn commit_sealed(
    conn: &Connection,
    report: &SealedResolutionReport,
    source: &str,
) -> Result<SealedCommitResult> {
    let mut added = 0u32;
    for r in &report.matched {
        sealed::add(
            conn,
            &NewSealed {
                product_id: r.product_id,
                quantity: Some(i64::from(r.quantity)),
                condition: Some(r.condition.clone()),
                purchase_price: r.purchase_price,
                purchase_date: r.purchase_date.clone(),
                source: Some(source.to_string()),
                seller_name: None,
                notes: r.notes.clone(),
            },
        )?;
        added += 1;
    }
    Ok(SealedCommitResult {
        added,
        skipped: report.unmatched.len() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, ptcgo_code, name, series) \
                 VALUES ('jtg', 'JTG', 'Journey Together', 'SV')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO sealed_products (product_id, set_code, name, category, fetched_at) \
                 VALUES \
                   (6001, 'jtg', 'Journey Together Elite Trainer Box', 'elite_trainer_box', '2026-02-28'), \
                   (6002, 'jtg', 'Journey Together Booster Bundle', 'booster_bundle', '2026-02-28'), \
                   (6003, 'jtg', 'Journey Together 3 Pack Blister [Scrafty]', 'blister', '2026-02-28'), \
                   (6004, 'jtg', 'Journey Together 3 Pack Blister [Yanmega]', 'blister', '2026-02-28')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn row(name: &str, set: &str, qty: u32) -> ParsedSealedRow {
        ParsedSealedRow {
            source_line: 2,
            name: name.to_string(),
            set_hint: set.to_string(),
            set_name: Some(set.to_string()),
            category_hint: None,
            quantity: qty,
            condition: "Near Mint".to_string(),
            purchase_price: Some(75.0),
            purchase_date: Some("2026-02-28".to_string()),
            notes: None,
        }
    }

    #[test]
    fn resolves_exact_name_and_keeps_quantity() {
        let (_d, conn) = db();
        let rows = vec![row(
            "Journey Together Elite Trainer Box",
            "Journey Together",
            3,
        )];
        let report = resolve_sealed(&conn, &rows).unwrap();
        assert_eq!(report.matched.len(), 1);
        assert_eq!(report.matched[0].product_id, 6001);
        assert_eq!(report.matched[0].quantity, 3); // not expanded to 3 rows
        assert!(report.unmatched.is_empty());
    }

    #[test]
    fn commit_writes_one_row_with_quantity() {
        let (_d, conn) = db();
        let rows = vec![row(
            "Journey Together Elite Trainer Box",
            "Journey Together",
            3,
        )];
        let report = resolve_sealed(&conn, &rows).unwrap();
        let result = commit_sealed(&conn, &report, "csv_collectr").unwrap();
        assert_eq!(result.added, 1);

        let entries = sealed::list(&conn).unwrap();
        assert_eq!(entries.len(), 1); // one row, quantity 3 — sealed aggregates
        assert_eq!(entries[0].quantity, 3);
        assert_eq!(entries[0].source.as_deref(), Some("csv_collectr"));
        assert_eq!(entries[0].purchase_date.as_deref(), Some("2026-02-28"));
    }

    #[test]
    fn unknown_set_and_unknown_product_are_reported() {
        let (_d, conn) = db();
        let rows = vec![
            row("Some Box", "No Such Set", 1),
            row("Nonexistent Product", "Journey Together", 1),
        ];
        let report = resolve_sealed(&conn, &rows).unwrap();
        assert!(report.matched.is_empty());
        assert_eq!(report.unmatched.len(), 2);
        // Unknown set: the global fallback still finds nothing, and the
        // reason names both the missing product and the unresolved set.
        assert!(
            report.unmatched[0].reason.contains("any set"),
            "{:?}",
            report.unmatched[0]
        );
        assert!(report.unmatched[0].reason.contains("No Such Set"));
        // Known set, no such product anywhere: still a specific miss.
        assert!(report.unmatched[1].reason.contains("no sealed product"));
    }

    #[test]
    fn distinct_blister_variants_do_not_collide() {
        let (_d, conn) = db();
        // The two 3-pack blisters differ only by their bracketed mascot;
        // an exact normalized match must pick the right one, not go ambiguous.
        let rows = vec![row(
            "Journey Together 3 Pack Blister [Yanmega]",
            "Journey Together",
            4,
        )];
        let report = resolve_sealed(&conn, &rows).unwrap();
        assert_eq!(report.matched.len(), 1, "{:?}", report.unmatched);
        assert_eq!(report.matched[0].product_id, 6004);
    }

    /// A catalog with NULL-set-code products (UPCs, Tin Displays), reachable
    /// only through the global name fallback (pokedumpster-oq3i.3).
    fn null_set_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, ptcgo_code, name, series) \
                 VALUES ('sv8pt5', 'PRE', 'Prismatic Evolutions', 'SV')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO sealed_products (product_id, set_code, name, category, fetched_at) VALUES \
                   (654213, NULL, 'Mega Charizard X ex Ultra Premium Collection', 'upc', '2026-02-01'), \
                   (656997, NULL, 'Team Rocket’s Moltres ex Ultra-Premium Collection', 'upc', '2026-02-01'), \
                   (622770, 'sv8pt5', 'Prismatic Evolutions Super-Premium Collection', 'spc', '2026-02-01')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn misc_sealed(name: &str) -> ParsedSealedRow {
        ParsedSealedRow {
            source_line: 2,
            name: name.to_string(),
            set_hint: "Miscellaneous Cards & Products".to_string(),
            set_name: Some("Miscellaneous Cards & Products".to_string()),
            category_hint: None,
            quantity: 1,
            condition: "Near Mint".to_string(),
            purchase_price: None,
            purchase_date: None,
            notes: None,
        }
    }

    #[test]
    fn null_set_code_product_resolves_by_global_name() {
        // Set 'Miscellaneous Cards & Products' doesn't resolve; the product
        // lives at NULL set_code and is only reachable via the global search.
        let (_d, conn) = null_set_db();
        let report = resolve_sealed(
            &conn,
            &[misc_sealed("Mega Charizard X ex Ultra Premium Collection")],
        )
        .unwrap();
        assert_eq!(report.matched.len(), 1, "{:?}", report.unmatched);
        assert_eq!(report.matched[0].product_id, 654213);
        assert_eq!(report.matched[0].set_code, None);
    }

    #[test]
    fn global_fallback_folds_hyphens_and_apostrophes() {
        let (_d, conn) = null_set_db();
        // Collectr "Super Premium" (space) vs catalog "Super-Premium" (hyphen).
        let r1 = resolve_sealed(
            &conn,
            &[misc_sealed("Prismatic Evolutions Super Premium Collection")],
        )
        .unwrap();
        assert_eq!(r1.matched.len(), 1, "{:?}", r1.unmatched);
        assert_eq!(r1.matched[0].product_id, 622770);

        // Straight apostrophe + spaced "Ultra Premium" vs catalog U+2019 +
        // hyphenated "Ultra-Premium".
        let r2 = resolve_sealed(
            &conn,
            &[misc_sealed(
                "Team Rocket's Moltres ex Ultra Premium Collection",
            )],
        )
        .unwrap();
        assert_eq!(r2.matched.len(), 1, "{:?}", r2.unmatched);
        assert_eq!(r2.matched[0].product_id, 656997);
    }
}
