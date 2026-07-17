//! CSV import: resolve `pkdump-core`-parsed rows against the catalog into
//! a preview report, then commit the matched rows (PLAN.md §9).

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension};

use pkdump_core::import::collectr;
use pkdump_core::import::{self, ParsedRow};

use crate::batches::{self, NewBatch};
use crate::collection::{self, CopyEdit, NewCopy};
use crate::error::{DbError, Result};
use crate::sealed_import::{self, SealedCommitResult, SealedResolutionReport};

/// A supported import file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportFormat {
    Manabox,
    Tcgplayer,
    /// PokeDumpster-native — what the pkmn.gg Tampermonkey export writes.
    Pokedumpster,
    /// Collectr (getcollectr.com). Yields both singles and sealed products,
    /// so it goes through the combined path ([`preview_collectr`] /
    /// [`commit_collectr`]), never the singles-only [`parse`].
    Collectr,
}

impl ImportFormat {
    /// Parse the wire name (`"manabox"`, `"tcgplayer"`, `"pokedumpster"`,
    /// `"collectr"`). `"pkmngg"` is accepted as an alias for `"pokedumpster"`
    /// since that is currently the only producer of the format.
    pub fn parse(name: &str) -> Result<Self> {
        match name.trim().to_lowercase().as_str() {
            "manabox" => Ok(Self::Manabox),
            "tcgplayer" => Ok(Self::Tcgplayer),
            "pokedumpster" | "pkmngg" => Ok(Self::Pokedumpster),
            "collectr" => Ok(Self::Collectr),
            other => Err(DbError::Import(format!("unknown import format '{other}'"))),
        }
    }

    /// The `collection.source` value for copies imported in this format.
    fn source(self) -> &'static str {
        match self {
            Self::Manabox => "csv_manabox",
            Self::Tcgplayer => "csv_tcgplayer",
            Self::Pokedumpster => "csv_pokedumpster",
            Self::Collectr => "csv_collectr",
        }
    }

    /// The `batches.batch_type` value for this format's import batches.
    fn batch_type(self) -> &'static str {
        match self {
            Self::Manabox => "csv_manabox",
            Self::Tcgplayer => "csv_tcgplayer",
            Self::Pokedumpster => "csv_pokedumpster",
            Self::Collectr => "csv_collectr",
        }
    }
}

/// Parse CSV `content` in the given format into pre-resolution single-card
/// rows. Collectr is rejected here: it also yields sealed products, so it
/// must go through [`preview_collectr`] / [`commit_collectr`].
fn parse(format: ImportFormat, content: &str) -> Result<Vec<ParsedRow>> {
    Ok(match format {
        ImportFormat::Manabox => import::manabox::parse(content)?,
        ImportFormat::Tcgplayer => import::tcgplayer::parse(content)?,
        ImportFormat::Pokedumpster => import::pokedumpster::parse(content)?,
        ImportFormat::Collectr => {
            return Err(DbError::Import(
                "Collectr yields sealed products too; use the combined import path".into(),
            ));
        }
    })
}

/// A row that resolved cleanly to a catalog printing — ready to commit.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ResolvedRow {
    #[ts(type = "number")]
    pub source_line: u32,
    pub printing_id: String,
    pub card_name: String,
    pub set_code: String,
    pub number: String,
    pub variant: String,
    pub condition: String,
    pub language: String,
    pub purchase_price: Option<f64>,
    pub acquired_at: Option<String>,
    pub tags: Vec<String>,
}

/// A row that could not be resolved, with a human-readable reason.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct UnmatchedRow {
    #[ts(type = "number")]
    pub source_line: u32,
    pub set_hint: String,
    pub number: String,
    pub variant: String,
    pub reason: String,
}

/// The outcome of resolving an import file — a preview shown before commit.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ResolutionReport {
    pub matched: Vec<ResolvedRow>,
    pub unmatched: Vec<UnmatchedRow>,
}

/// The outcome of committing an import.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CommitResult {
    #[ts(type = "number")]
    pub batch_id: i64,
    #[ts(type = "number")]
    pub added: u32,
    #[ts(type = "number")]
    pub skipped: u32,
}

/// Resolve a set hint to a catalog `set_code`: exact code, then ptcgo code,
/// then set name (the explicit name field, then the hint read as a name).
///
/// Shared with the sealed import pipeline ([`crate::sealed_import`]).
pub(crate) fn resolve_set(
    conn: &Connection,
    hint: &str,
    name: Option<&str>,
) -> Result<Option<String>> {
    let by = |sql: &str, value: &str| -> Result<Option<String>> {
        Ok(conn
            .prepare(sql)?
            .query_row([value], |r| r.get(0))
            .optional()?)
    };
    if let Some(c) = by(
        "SELECT set_code FROM sets WHERE set_code = ?1 COLLATE NOCASE",
        hint,
    )? {
        return Ok(Some(c));
    }
    if let Some(c) = by(
        "SELECT set_code FROM sets WHERE ptcgo_code = ?1 COLLATE NOCASE",
        hint,
    )? {
        return Ok(Some(c));
    }
    if let Some(n) = name
        && let Some(c) = by(
            "SELECT set_code FROM sets WHERE name = ?1 COLLATE NOCASE",
            n,
        )?
    {
        return Ok(Some(c));
    }
    by(
        "SELECT set_code FROM sets WHERE name = ?1 COLLATE NOCASE",
        hint,
    )
}

/// Resolve parsed rows against the catalog, partitioning them into matched
/// printings and unmatched rows with reasons.
pub fn resolve(conn: &Connection, rows: &[ParsedRow]) -> Result<ResolutionReport> {
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    // A CSV usually touches a handful of sets — cache the hint→code lookup.
    let mut set_cache: HashMap<(String, Option<String>), Option<String>> = HashMap::new();

    for row in rows {
        let line = row.source_line as u32;
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
            unmatched.push(UnmatchedRow {
                source_line: line,
                set_hint: row.set_hint.clone(),
                number: row.number.clone(),
                variant: row.variant.clone(),
                reason: format!("unknown set '{}'", row.set_hint),
            });
            continue;
        };

        let card: Option<(String, String)> = conn
            .prepare("SELECT card_id, name FROM cards WHERE set_code = ?1 AND number = ?2")?
            .query_row((&set_code, &row.number), |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((card_id, card_name)) = card else {
            unmatched.push(UnmatchedRow {
                source_line: line,
                set_hint: row.set_hint.clone(),
                number: row.number.clone(),
                variant: row.variant.clone(),
                reason: format!("card #{} not found in {set_code}", row.number),
            });
            continue;
        };

        let printing_id: Option<String> = conn
            .prepare(
                "SELECT printing_id FROM printings \
                 WHERE card_id = ?1 AND variant = ?2 AND deprecated_at IS NULL",
            )?
            .query_row((&card_id, &row.variant), |r| r.get(0))
            .optional()?;
        let Some(printing_id) = printing_id else {
            unmatched.push(UnmatchedRow {
                source_line: line,
                set_hint: row.set_hint.clone(),
                number: row.number.clone(),
                variant: row.variant.clone(),
                reason: format!("variant '{}' not available for {card_name}", row.variant),
            });
            continue;
        };

        matched.push(ResolvedRow {
            source_line: line,
            printing_id,
            card_name,
            set_code,
            number: row.number.clone(),
            variant: row.variant.clone(),
            condition: row.condition.clone(),
            language: row.language.clone(),
            purchase_price: row.purchase_price,
            acquired_at: row.acquired_at.clone(),
            tags: row.tags.clone(),
        });
    }
    Ok(ResolutionReport { matched, unmatched })
}

/// A JSON array literal for tag strings. Tags are simple identifiers
/// (`misprint`, `altered`) so no escaping is needed.
fn tags_json(tags: &[String]) -> String {
    let items: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
    format!("[{}]", items.join(","))
}

/// Preview an import: parse and resolve without writing anything.
pub fn preview(conn: &Connection, format: ImportFormat, content: &str) -> Result<ResolutionReport> {
    let rows = parse(format, content)?;
    resolve(conn, &rows)
}

/// Commit an import: parse, resolve, then add every matched row under a
/// fresh batch. Re-resolves server-side rather than trusting a preview.
pub fn commit(
    conn: &mut Connection,
    format: ImportFormat,
    content: &str,
    batch_name: Option<&str>,
) -> Result<CommitResult> {
    let rows = parse(format, content)?;
    let report = resolve(conn, &rows)?;
    commit_matched(
        conn,
        &report,
        format.source(),
        format.batch_type(),
        batch_name,
    )
}

/// Add every matched single-card row under a fresh batch. Shared by the
/// per-format [`commit`] and the combined [`commit_collectr`].
fn commit_matched(
    conn: &mut Connection,
    report: &ResolutionReport,
    source: &str,
    batch_type: &str,
    batch_name: Option<&str>,
) -> Result<CommitResult> {
    let batch_id = batches::create(
        conn,
        &NewBatch {
            batch_type: batch_type.to_string(),
            name: batch_name.map(str::to_string),
            ..Default::default()
        },
    )?;

    let mut added = 0u32;
    for r in &report.matched {
        let id = collection::add(
            conn,
            &NewCopy {
                printing_id: r.printing_id.clone(),
                condition: Some(r.condition.clone()),
                language: Some(r.language.clone()),
                purchase_price: r.purchase_price,
                acquired_at: r.acquired_at.clone(),
                source: source.to_string(),
                batch_id: Some(batch_id),
                ..Default::default()
            },
        )?;
        if !r.tags.is_empty() {
            collection::update(
                conn,
                id,
                &CopyEdit {
                    tags: Some(tags_json(&r.tags)),
                    ..Default::default()
                },
            )?;
        }
        added += 1;
    }

    Ok(CommitResult {
        batch_id,
        added,
        skipped: report.unmatched.len() as u32,
    })
}

// --- Collectr: the combined single + sealed import path (garden wall) ---

/// A row skipped during a combined import (e.g. a non-Pokémon Collectr row).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SkippedRow {
    #[ts(type = "number")]
    pub source_line: u32,
    pub category: String,
    pub name: String,
    pub reason: String,
}

impl From<collectr::SkippedRow> for SkippedRow {
    fn from(s: collectr::SkippedRow) -> Self {
        Self {
            source_line: s.source_line as u32,
            category: s.category,
            name: s.name,
            reason: s.reason,
        }
    }
}

/// The preview of a combined import: the single and sealed resolutions kept
/// strictly apart (the garden wall), plus rows dropped as out of scope.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CombinedReport {
    pub singles: ResolutionReport,
    pub sealed: SealedResolutionReport,
    pub skipped: Vec<SkippedRow>,
}

/// The outcome of committing a combined import.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CombinedCommitResult {
    pub singles: CommitResult,
    pub sealed: SealedCommitResult,
    #[ts(type = "number")]
    pub skipped: u32,
}

/// Preview a Collectr import: split the file, then resolve singles and
/// sealed independently. Writes nothing.
pub fn preview_collectr(conn: &Connection, content: &str) -> Result<CombinedReport> {
    let parsed = collectr::parse(content)?;
    let singles = resolve(conn, &parsed.singles)?;
    let sealed = sealed_import::resolve_sealed(conn, &parsed.sealed)?;
    Ok(CombinedReport {
        singles,
        sealed,
        skipped: parsed.skipped.into_iter().map(Into::into).collect(),
    })
}

/// Commit a Collectr import: split the file, re-resolve both halves
/// server-side, then write singles to `collection` (under a batch) and
/// sealed to `sealed_collection`. The two halves never cross.
pub fn commit_collectr(
    conn: &mut Connection,
    content: &str,
    batch_name: Option<&str>,
) -> Result<CombinedCommitResult> {
    let parsed = collectr::parse(content)?;
    let singles_report = resolve(conn, &parsed.singles)?;
    let sealed_report = sealed_import::resolve_sealed(conn, &parsed.sealed)?;

    let singles = commit_matched(
        conn,
        &singles_report,
        "csv_collectr",
        "csv_collectr",
        batch_name,
    )?;
    let sealed = sealed_import::commit_sealed(conn, &sealed_report, "csv_collectr")?;

    Ok(CombinedCommitResult {
        singles,
        sealed,
        skipped: parsed.skipped.len() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};

    const CSV: &str = "\
Set code,Set name,Collector number,Foil,Quantity,Condition,Language,Purchase price
MEW,151,6,normal,2,near_mint,en,1.50
MEW,151,6,foil,1,near_mint,en,5.00
NOPE,Mystery,1,normal,1,near_mint,en,";

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, ptcgo_code, name, series) \
                 VALUES ('sv3pt5', 'MEW', '151', 'SV')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-6-normal', 'sv3pt5-6', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn resolves_by_ptcgo_code_and_reports_misses() {
        let (_d, conn) = db();
        let report = preview(&conn, ImportFormat::Manabox, CSV).unwrap();

        // Two 'normal' copies of #6 resolve via the ptcgo code 'MEW'.
        assert_eq!(report.matched.len(), 2);
        assert_eq!(report.matched[0].set_code, "sv3pt5");
        assert_eq!(report.matched[0].card_name, "Charizard ex");
        assert_eq!(report.matched[0].purchase_price, Some(1.50));

        // The 'foil' copy (no holo printing) and the unknown set both miss.
        assert_eq!(report.unmatched.len(), 2);
        assert!(
            report
                .unmatched
                .iter()
                .any(|u| u.reason.contains("variant 'holo'"))
        );
        assert!(
            report
                .unmatched
                .iter()
                .any(|u| u.reason.contains("unknown set 'NOPE'"))
        );
    }

    #[test]
    fn commit_adds_matched_rows_under_a_batch() {
        let (_d, mut conn) = db();
        let result = commit(&mut conn, ImportFormat::Manabox, CSV, Some("import.csv")).unwrap();
        assert_eq!(result.added, 2);
        assert_eq!(result.skipped, 2);

        let cards = collection::list_by_batch(&conn, result.batch_id).unwrap();
        assert_eq!(cards.len(), 2);
        assert!(cards.iter().all(|c| c.source == "csv_manabox"));
    }

    #[test]
    fn unknown_format_is_rejected() {
        assert!(ImportFormat::parse("nonsense").is_err());
        // Collectr is now a recognized format.
        assert_eq!(
            ImportFormat::parse("collectr").unwrap(),
            ImportFormat::Collectr
        );
    }

    const COLLECTR_CSV: &str = "\
Portfolio Name,Category,Set,Product Name,Card Number,Rarity,Variance,Grade,Card Condition,Average Cost Paid,Quantity,Market Price (As of 2026-07-17),Price Override,Watchlist,Date Added,Notes
Main,Lorcana,Disney Lorcana Promo Cards,Elsa,6,Promo,Holofoil,Ungraded,Near Mint,0,1,7.12,0,false,2026-01-24,
Main,Pokemon,151,Charizard ex,6,Double Rare,Normal,Ungraded,Near Mint,1.50,2,5.00,0,false,2026-04-14,
Sealed Pokemon TCG,Pokemon,151,151 Elite Trainer Box,,,Normal,Ungraded,Near Mint,49.99,3,60.00,0,false,2026-02-28,";

    fn sealed_db() -> (tempfile::TempDir, Connection) {
        let (dir, conn) = {
            let dir = tempfile::tempdir().unwrap();
            let shared = dir.path().join("shared.sqlite");
            {
                let c = open_shared(&shared).unwrap();
                c.execute(
                    "INSERT INTO sets (set_code, ptcgo_code, name, series) \
                     VALUES ('sv3pt5', 'MEW', '151', 'SV')",
                    [],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex')",
                    [],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) \
                     VALUES ('sv3pt5-6-normal', 'sv3pt5-6', 'normal')",
                    [],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO sealed_products (product_id, set_code, name, category, fetched_at) \
                     VALUES (7001, 'sv3pt5', '151 Elite Trainer Box', 'elite_trainer_box', '2026-02-28')",
                    [],
                )
                .unwrap();
            }
            let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
            (dir, conn)
        };
        (dir, conn)
    }

    #[test]
    fn collectr_preview_splits_singles_sealed_and_skips() {
        let (_d, conn) = sealed_db();
        let report = preview_collectr(&conn, COLLECTR_CSV).unwrap();

        // Two 'normal' Charizard copies resolve; set '151' matches by name.
        assert_eq!(report.singles.matched.len(), 2);
        assert_eq!(report.singles.matched[0].card_name, "Charizard ex");
        assert_eq!(
            report.singles.matched[0].acquired_at.as_deref(),
            Some("2026-04-14T00:00:00Z")
        );
        // The ETB resolves on the sealed side, quantity 3 preserved.
        assert_eq!(report.sealed.matched.len(), 1);
        assert_eq!(report.sealed.matched[0].quantity, 3);
        // Lorcana row is skipped.
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].category, "Lorcana");
    }

    #[test]
    fn collectr_commit_writes_both_sides_without_crossing() {
        let (_d, mut conn) = sealed_db();
        let result = commit_collectr(&mut conn, COLLECTR_CSV, Some("collectr.csv")).unwrap();
        assert_eq!(result.singles.added, 2);
        assert_eq!(result.sealed.added, 1);
        assert_eq!(result.skipped, 1);

        // Singles landed in `collection`, sealed in `sealed_collection` —
        // one ETB row with quantity 3, never leaked into singles.
        let singles = collection::list_by_batch(&conn, result.singles.batch_id).unwrap();
        assert_eq!(singles.len(), 2);
        assert!(singles.iter().all(|c| c.source == "csv_collectr"));

        let sealed = crate::sealed::list(&conn).unwrap();
        assert_eq!(sealed.len(), 1);
        assert_eq!(sealed[0].quantity, 3);
    }
}
