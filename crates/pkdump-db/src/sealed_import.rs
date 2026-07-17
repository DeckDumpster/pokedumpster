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
/// and collapse runs of whitespace. Mirrors the spirit of the shared
/// column-value mappings in `pkdump-core::import`.
fn normalize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_ws = false;
    for c in name.chars() {
        let c = match c {
            '\u{2019}' | '\u{2018}' | '`' | '\u{02bc}' => '\'',
            'é' | 'É' | 'è' | 'ë' => 'e',
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

    for row in rows {
        let line = row.source_line as u32;
        let miss = |reason: String| UnmatchedSealedRow {
            source_line: line,
            name: row.name.clone(),
            set_hint: row.set_hint.clone(),
            reason,
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
        let Some(set_code) = set_code else {
            unmatched.push(miss(format!("unknown set '{}'", row.set_hint)));
            continue;
        };

        if !cand_cache.contains_key(&set_code) {
            let c = candidates_in_set(conn, &set_code)?;
            cand_cache.insert(set_code.clone(), c);
        }
        let candidates = &cand_cache[&set_code];

        match match_name(&row.name, candidates) {
            Match::One(c) => matched.push(ResolvedSealedRow {
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
            }),
            Match::Ambiguous(names) => unmatched.push(miss(format!(
                "ambiguous in {set_code}: matches {}",
                names.join(", ")
            ))),
            Match::None => unmatched.push(miss(format!(
                "no sealed product '{}' in {set_code}",
                row.name
            ))),
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
        assert!(report.unmatched[0].reason.contains("unknown set"));
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
}
