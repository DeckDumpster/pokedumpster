//! CSV import: resolve `pkdump-core`-parsed rows against the catalog into
//! a preview report, then commit the matched rows (PLAN.md §9).

use std::collections::{HashMap, HashSet};

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
    /// How many copies of this printing are already `owned` in the collection
    /// (duplicate flag for the import preview). Filled by [`annotate_owned`]
    /// after resolution; `0` straight out of [`resolve`]. (pokedumpster-oq3i.4)
    #[ts(type = "number")]
    pub already_owned: u32,
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
/// then set name (the explicit name field, then the hint read as a name),
/// and finally the external set-name alias table (`set_aliases`) so an
/// import platform's own label (Collectr's "Scarlet & Violet Promo") lands
/// on the catalog code ('svp'). Aliases are data — see [`crate::set_aliases`].
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
    if let Some(c) = by(
        "SELECT set_code FROM sets WHERE name = ?1 COLLATE NOCASE",
        hint,
    )? {
        return Ok(Some(c));
    }
    // Fallback: an external platform's set label, mapped by the alias table.
    let alias_sql = "SELECT set_code FROM set_aliases WHERE alias = ?1";
    if let Some(n) = name
        && let Some(c) = by(alias_sql, n)?
    {
        return Ok(Some(c));
    }
    by(alias_sql, hint)
}

/// Strip trailing parenthetical/bracketed descriptor groups an import
/// platform appends to a card name that the catalog name doesn't carry —
/// "Great Tusk ex (Paldean Fates Stamped)" → "Great Tusk ex", "Ancient Mew
/// [2000]" → "Ancient Mew". Only *trailing* groups are removed (repeatedly),
/// so interior text is untouched; the catalog side is never stripped, so a
/// genuinely distinct printing like "Ancient Mew (Japanese Exclusive Print)"
/// stays its own card.
fn strip_trailing_descriptors(name: &str) -> String {
    let mut s = name.trim();
    loop {
        let open = match s.chars().last() {
            Some(')') => '(',
            Some(']') => '[',
            _ => break,
        };
        match s.rfind(open) {
            Some(pos) => s = s[..pos].trim_end(),
            None => break,
        }
    }
    s.to_string()
}

/// The outcome of the global (all-sets) card name+number fallback.
enum CardMatch {
    One {
        card_id: String,
        card_name: String,
        set_code: String,
    },
    /// The name+number matches several catalog cards — the caller reports it
    /// for manual resolution rather than guessing.
    Ambiguous(Vec<String>),
    None,
}

/// Search the whole card catalog by (folded name, normalized number) when a
/// row's stated set didn't resolve or the card wasn't in it — Collectr's
/// "Miscellaneous Cards & Products" catch-all, stamped promos, etc. The name
/// is stripped of trailing descriptors and folded the same way sealed names
/// are; the number is already leading-zero-normalized by the caller.
fn global_card_fallback(conn: &Connection, want_name: &str, number: &str) -> Result<CardMatch> {
    let want = crate::sealed_import::normalize_name(&strip_trailing_descriptors(want_name));
    if want.is_empty() {
        return Ok(CardMatch::None);
    }
    let mut stmt = conn.prepare("SELECT card_id, name, set_code FROM cards WHERE number = ?1")?;
    let mut hits: Vec<(String, String, String)> = stmt
        .query_map([number], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<Vec<(String, String, String)>>>()?
        .into_iter()
        .filter(|(_, name, _)| crate::sealed_import::normalize_name(name) == want)
        .collect();

    match hits.len() {
        0 => Ok(CardMatch::None),
        1 => {
            let (card_id, card_name, set_code) = hits.pop().unwrap();
            Ok(CardMatch::One {
                card_id,
                card_name,
                set_code,
            })
        }
        _ => Ok(CardMatch::Ambiguous(
            hits.iter()
                .map(|(_, _, sc)| format!("{sc} #{number}"))
                .collect(),
        )),
    }
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
        let unmatched_row = |reason: String| UnmatchedRow {
            source_line: line,
            set_hint: row.set_hint.clone(),
            number: row.number.clone(),
            variant: row.variant.clone(),
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

        // The catalog stores collector numbers unpadded ('90'); Collectr
        // zero-pads them ('090'). Normalize both to the same canonical form
        // so an exact `number =` lookup lands.
        let lookup_number = import::normalize_collector_number(&row.number);

        // Direct set+number lookup when the set resolved.
        let direct: Option<(String, String)> = match &set_code {
            Some(sc) => conn
                .prepare("SELECT card_id, name FROM cards WHERE set_code = ?1 AND number = ?2")?
                .query_row((sc, &lookup_number), |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?,
            None => None,
        };

        // On a direct miss, fall back to a global name+number search across
        // all sets (the "Miscellaneous Cards & Products" catch-all, stamped
        // promos, and anything whose stated set didn't resolve). Only rows
        // that carry a card name can use it.
        let (card_id, card_name, resolved_set) = match direct {
            Some((id, name)) => (id, name, set_code.clone().expect("set_code present")),
            None => match row.name.as_deref() {
                Some(want) => match global_card_fallback(conn, want, &lookup_number)? {
                    CardMatch::One {
                        card_id,
                        card_name,
                        set_code,
                    } => (card_id, card_name, set_code),
                    CardMatch::Ambiguous(cands) => {
                        unmatched.push(unmatched_row(format!(
                            "'{want}' #{lookup_number} is ambiguous: matches {}",
                            cands.join(", ")
                        )));
                        continue;
                    }
                    CardMatch::None => {
                        unmatched.push(unmatched_row(match &set_code {
                            Some(sc) => format!(
                                "card #{lookup_number} not found in {sc}; \
                                 no '{want}' #{lookup_number} in any set"
                            ),
                            None => format!(
                                "no card '{want}' #{lookup_number} in any set \
                                 (set '{}' unknown)",
                                row.set_hint
                            ),
                        }));
                        continue;
                    }
                },
                None => {
                    unmatched.push(unmatched_row(match &set_code {
                        Some(sc) => format!("card #{lookup_number} not found in {sc}"),
                        None => format!("unknown set '{}'", row.set_hint),
                    }));
                    continue;
                }
            },
        };

        let printing_id: Option<String> = conn
            .prepare(
                "SELECT printing_id FROM printings \
                 WHERE card_id = ?1 AND variant = ?2 AND deprecated_at IS NULL",
            )?
            .query_row((&card_id, &row.variant), |r| r.get(0))
            .optional()?;
        let Some(printing_id) = printing_id else {
            unmatched.push(unmatched_row(format!(
                "variant '{}' not available for {card_name}",
                row.variant
            )));
            continue;
        };

        matched.push(ResolvedRow {
            source_line: line,
            printing_id,
            card_name,
            set_code: resolved_set,
            number: lookup_number,
            variant: row.variant.clone(),
            condition: row.condition.clone(),
            language: row.language.clone(),
            purchase_price: row.purchase_price,
            acquired_at: row.acquired_at.clone(),
            tags: row.tags.clone(),
            already_owned: 0, // filled by annotate_owned() (oq3i.4)
        });
    }
    Ok(ResolutionReport { matched, unmatched })
}

/// Annotate each matched single-card row with how many `owned` copies of the
/// same printing already sit in the collection, so the import preview can flag
/// duplicates the user may want to deselect. Purely additive: runs *after*
/// [`resolve`], mutating the report in place. (pokedumpster-oq3i.4)
pub fn annotate_owned(conn: &Connection, report: &mut ResolutionReport) -> Result<()> {
    if report.matched.is_empty() {
        return Ok(());
    }
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT printing_id, COUNT(*) FROM collection \
         WHERE status = 'owned' GROUP BY printing_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (printing_id, n) = row?;
        counts.insert(printing_id, n as u32);
    }
    for m in &mut report.matched {
        m.already_owned = counts.get(&m.printing_id).copied().unwrap_or(0);
    }
    Ok(())
}

/// Annotate each matched sealed row with how many `owned` units of the same
/// product already sit in `sealed_collection` (quantity summed). Mirror of
/// [`annotate_owned`] for the sealed half. (pokedumpster-oq3i.4)
pub fn annotate_owned_sealed(conn: &Connection, report: &mut SealedResolutionReport) -> Result<()> {
    if report.matched.is_empty() {
        return Ok(());
    }
    let mut counts: HashMap<i64, u32> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT product_id, COALESCE(SUM(quantity), 0) FROM sealed_collection \
         WHERE status = 'owned' GROUP BY product_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (product_id, n) = row?;
        counts.insert(product_id, n as u32);
    }
    for m in &mut report.matched {
        m.already_owned = counts.get(&m.product_id).copied().unwrap_or(0);
    }
    Ok(())
}

/// Keep only the matched rows whose `source_line` is in `include`; drop all
/// unmatched. Used by the selected-commit path so the user's per-row choices
/// in the preview are honored server-side (after a fresh re-resolve).
fn filter_report(report: &ResolutionReport, include: &[u32]) -> ResolutionReport {
    let keep: HashSet<u32> = include.iter().copied().collect();
    ResolutionReport {
        matched: report
            .matched
            .iter()
            .filter(|r| keep.contains(&r.source_line))
            .cloned()
            .collect(),
        unmatched: Vec::new(),
    }
}

/// Sealed mirror of [`filter_report`].
fn filter_sealed(report: &SealedResolutionReport, include: &[u32]) -> SealedResolutionReport {
    let keep: HashSet<u32> = include.iter().copied().collect();
    SealedResolutionReport {
        matched: report
            .matched
            .iter()
            .filter(|r| keep.contains(&r.source_line))
            .cloned()
            .collect(),
        unmatched: Vec::new(),
    }
}

/// A JSON array literal for tag strings. Tags are simple identifiers
/// (`misprint`, `altered`) so no escaping is needed.
pub(crate) fn tags_json(tags: &[String]) -> String {
    let items: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
    format!("[{}]", items.join(","))
}

/// Preview an import: parse and resolve without writing anything.
pub fn preview(conn: &Connection, format: ImportFormat, content: &str) -> Result<ResolutionReport> {
    let rows = parse(format, content)?;
    let mut report = resolve(conn, &rows)?;
    annotate_owned(conn, &mut report)?;
    Ok(report)
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

/// Commit only the selected matched rows of a single-card import. Re-resolves
/// server-side (never trusts the preview) then keeps only the rows whose
/// `source_line` the user left selected. (pokedumpster-oq3i.4)
///
/// When `park_unmatched` is set, the rows that didn't resolve at all are parked
/// in the dead-letter queue under this commit's batch, to resolve later
/// (pokedumpster-oq3i.5).
pub fn commit_selected(
    conn: &mut Connection,
    format: ImportFormat,
    content: &str,
    include: &[u32],
    batch_name: Option<&str>,
    park_unmatched: bool,
) -> Result<CommitResult> {
    let rows = parse(format, content)?;
    let full = resolve(conn, &rows)?;
    let report = filter_report(&full, include);
    let result = commit_matched(
        conn,
        &report,
        format.source(),
        format.batch_type(),
        batch_name,
    )?;
    if park_unmatched {
        crate::unresolved::park_report(
            conn,
            format.source(),
            Some(result.batch_id),
            &rows,
            &full,
            &[],
            &SealedResolutionReport {
                matched: Vec::new(),
                unmatched: Vec::new(),
            },
        )?;
    }
    Ok(result)
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
    let mut singles = resolve(conn, &parsed.singles)?;
    annotate_owned(conn, &mut singles)?;
    let mut sealed = sealed_import::resolve_sealed(conn, &parsed.sealed)?;
    annotate_owned_sealed(conn, &mut sealed)?;
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

/// Commit only the selected rows of a Collectr import. Re-resolves both halves
/// server-side, keeps only the `source_line`s the user selected in each pane,
/// then writes singles and sealed to their own tables (garden wall intact).
/// (pokedumpster-oq3i.4)
pub fn commit_collectr_selected(
    conn: &mut Connection,
    content: &str,
    include_singles: &[u32],
    include_sealed: &[u32],
    batch_name: Option<&str>,
    park_unmatched: bool,
) -> Result<CombinedCommitResult> {
    let parsed = collectr::parse(content)?;
    let singles_full = resolve(conn, &parsed.singles)?;
    let sealed_full = sealed_import::resolve_sealed(conn, &parsed.sealed)?;
    let singles_report = filter_report(&singles_full, include_singles);
    let sealed_report = filter_sealed(&sealed_full, include_sealed);

    let singles = commit_matched(
        conn,
        &singles_report,
        "csv_collectr",
        "csv_collectr",
        batch_name,
    )?;
    let sealed = sealed_import::commit_sealed(conn, &sealed_report, "csv_collectr")?;

    if park_unmatched {
        // Both halves' misses go to the queue under the singles batch (the
        // import's provenance handle).
        crate::unresolved::park_report(
            conn,
            "csv_collectr",
            Some(singles.batch_id),
            &parsed.singles,
            &singles_full,
            &parsed.sealed,
            &sealed_full,
        )?;
    }

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
            // A card stored at the unpadded number '90' — the leading-zero
            // regression fixture (Collectr writes '090').
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-90', 'sv3pt5', '90', 90, 'Simisage')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-90-normal', 'sv3pt5-90', 'normal')",
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
    fn zero_padded_number_resolves_against_unpadded_catalog() {
        // Collectr writes '090'; the catalog stores '90'. An exact WHERE
        // number='090' misses — the resolver must normalize both sides.
        let (_d, conn) = db();
        let rows = vec![ParsedRow {
            source_line: 2,
            set_hint: "sv3pt5".into(),
            set_name: None,
            name: None,
            number: "090".into(),
            variant: "normal".into(),
            condition: "Near Mint".into(),
            language: "English".into(),
            purchase_price: None,
            acquired_at: None,
            tags: Vec::new(),
        }];
        let report = resolve(&conn, &rows).unwrap();
        assert_eq!(report.matched.len(), 1, "{:?}", report.unmatched);
        assert_eq!(report.matched[0].card_name, "Simisage");
        assert_eq!(report.matched[0].printing_id, "sv3pt5-90-normal");
    }

    #[test]
    fn set_alias_plus_leading_zero_resolves_promo_card() {
        // Collectr sends set 'Mega Evolution Promos' (aliased to mep) and a
        // zero-padded number '009'; the catalog has mep #9 Alakazam. Both the
        // alias fallback and the leading-zero fix must fire together.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let mut c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('mep', 'ME Black Star Promos', 'SV')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('mep-9', 'mep', '9', 9, 'Alakazam')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('mep-9-normal', 'mep-9', 'normal')",
                [],
            )
            .unwrap();
            // Now that the mep set exists, the JSON seed writes its alias.
            let n = crate::set_aliases::reconcile(&mut c).unwrap();
            assert!(n >= 1, "expected the mep alias to seed once its set exists");
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();

        // resolve_set consults the alias table as a fallback.
        assert_eq!(
            resolve_set(
                &conn,
                "Mega Evolution Promos",
                Some("Mega Evolution Promos")
            )
            .unwrap(),
            Some("mep".to_string())
        );

        let rows = vec![ParsedRow {
            source_line: 2,
            set_hint: "Mega Evolution Promos".into(),
            set_name: Some("Mega Evolution Promos".into()),
            name: None,
            number: "009".into(),
            variant: "normal".into(),
            condition: "Near Mint".into(),
            language: "English".into(),
            purchase_price: None,
            acquired_at: None,
            tags: Vec::new(),
        }];
        let report = resolve(&conn, &rows).unwrap();
        assert_eq!(report.matched.len(), 1, "{:?}", report.unmatched);
        assert_eq!(report.matched[0].set_code, "mep");
        assert_eq!(report.matched[0].card_name, "Alakazam");
    }

    /// A catalog with the catch-all cards used by the fallback tests.
    fn misc_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES \
                   ('mcap', 'Miscellaneous Promos', 'Other'), \
                   ('svp', 'Scarlet & Violet Black Star Promos', 'SV'), \
                   ('sv4pt5', 'Paldean Fates', 'SV')",
                [],
            )
            .unwrap();
            // Ancient Mew at #1, and a distinct Japanese-exclusive printing at
            // #1a — the catalog side keeps its "(...)" so it stays separate.
            // Great Tusk ex #72 in svp vs Oinkologne #72 in sv4pt5 (same
            // number, different name → the name disambiguates).
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) VALUES \
                   ('mcap-1',  'mcap',   '1',  1, 'Ancient Mew'), \
                   ('mcap-1a', 'mcap',   '1a', 1, 'Ancient Mew (Japanese Exclusive Print)'), \
                   ('svp-72',  'svp',    '72', 72, 'Great Tusk ex'), \
                   ('sv4pt5-72','sv4pt5','72', 72, 'Oinkologne')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) VALUES \
                   ('mcap-1-normal', 'mcap-1', 'normal'), \
                   ('svp-72-holo', 'svp-72', 'holo')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn misc_row(name: &str, number: &str, variant: &str) -> ParsedRow {
        ParsedRow {
            source_line: 2,
            set_hint: "Miscellaneous Cards & Products".into(),
            set_name: Some("Miscellaneous Cards & Products".into()),
            name: Some(name.into()),
            number: number.into(),
            variant: variant.into(),
            condition: "Near Mint".into(),
            language: "English".into(),
            purchase_price: None,
            acquired_at: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn misc_card_resolves_by_name_stripping_year_suffix() {
        // Collectr's Misc bucket doesn't resolve as a set; the name (with its
        // "[2000]" suffix stripped) plus number falls back to mcap #1, not the
        // distinct "(Japanese Exclusive Print)" at #1a.
        let (_d, conn) = misc_db();
        let report = resolve(&conn, &[misc_row("Ancient Mew [2000]", "1", "normal")]).unwrap();
        assert_eq!(report.matched.len(), 1, "{:?}", report.unmatched);
        assert_eq!(report.matched[0].card_name, "Ancient Mew");
        assert_eq!(report.matched[0].set_code, "mcap");
        assert_eq!(report.matched[0].printing_id, "mcap-1-normal");
    }

    #[test]
    fn stamped_promo_resolves_by_name_and_number_across_sets() {
        // "Great Tusk ex (Paldean Fates Stamped)" #072 → svp #72 (leading-zero
        // strip + descriptor strip), never sv4pt5 #72 (Oinkologne).
        let (_d, conn) = misc_db();
        let report = resolve(
            &conn,
            &[misc_row(
                "Great Tusk ex (Paldean Fates Stamped)",
                "072",
                "holo",
            )],
        )
        .unwrap();
        assert_eq!(report.matched.len(), 1, "{:?}", report.unmatched);
        assert_eq!(report.matched[0].set_code, "svp");
        assert_eq!(report.matched[0].number, "72");
        assert_eq!(report.matched[0].printing_id, "svp-72-holo");
    }

    #[test]
    fn ambiguous_name_number_is_flagged_not_guessed() {
        // Two catalog cards share a (name, number): the fallback must report
        // the candidates for manual resolution rather than pick one.
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES \
                   ('seta', 'Set A', 'X'), ('setb', 'Set B', 'X')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) VALUES \
                   ('seta-5', 'seta', '5', 5, 'Pikachu'), \
                   ('setb-5', 'setb', '5', 5, 'Pikachu')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let report = resolve(&conn, &[misc_row("Pikachu", "5", "normal")]).unwrap();
        assert!(report.matched.is_empty());
        assert_eq!(report.unmatched.len(), 1);
        let reason = &report.unmatched[0].reason;
        assert!(reason.contains("ambiguous"), "{reason}");
        assert!(
            reason.contains("seta #5") && reason.contains("setb #5"),
            "{reason}"
        );
    }

    #[test]
    fn preview_flags_already_owned_copies() {
        let (_d, mut conn) = db();
        // Nothing owned yet — every matched row reads 0.
        let report = preview(&conn, ImportFormat::Manabox, CSV).unwrap();
        assert!(report.matched.iter().all(|r| r.already_owned == 0));

        // Own one copy of the normal printing, then re-preview.
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-6-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let report = preview(&conn, ImportFormat::Manabox, CSV).unwrap();
        assert!(
            report
                .matched
                .iter()
                .all(|r| r.printing_id != "sv3pt5-6-normal" || r.already_owned == 1),
            "matched normal rows should report already_owned == 1"
        );
    }

    #[test]
    fn commit_selected_adds_only_included_lines() {
        let (_d, mut conn) = db();
        // Selecting nothing adds nothing.
        let none =
            commit_selected(&mut conn, ImportFormat::Manabox, CSV, &[], None, false).unwrap();
        assert_eq!(none.added, 0);

        // Line 2 is the pair of matched 'normal' copies; including it adds both.
        let some = commit_selected(
            &mut conn,
            ImportFormat::Manabox,
            CSV,
            &[2],
            Some("sel.csv"),
            false,
        )
        .unwrap();
        assert_eq!(some.added, 2);
        let cards = collection::list_by_batch(&conn, some.batch_id).unwrap();
        assert_eq!(cards.len(), 2);
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

    #[test]
    fn collectr_preview_annotates_owned_on_both_sides() {
        let (_d, mut conn) = sealed_db();
        // Own one Charizard copy and the ETB up front.
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-6-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();
        crate::sealed::add(
            &conn,
            &crate::sealed::NewSealed {
                product_id: 7001,
                quantity: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

        let report = preview_collectr(&conn, COLLECTR_CSV).unwrap();
        assert!(report.singles.matched.iter().all(|r| r.already_owned == 1));
        assert_eq!(report.sealed.matched[0].already_owned, 2);
    }

    #[test]
    fn collectr_commit_selected_honors_each_pane() {
        let (_d, mut conn) = sealed_db();
        // Take the sealed line (4) only; leave the singles line (3) out.
        let r = commit_collectr_selected(&mut conn, COLLECTR_CSV, &[], &[4], None, false).unwrap();
        assert_eq!(r.singles.added, 0);
        assert_eq!(r.sealed.added, 1);

        // Now take only the singles line.
        let r = commit_collectr_selected(&mut conn, COLLECTR_CSV, &[3], &[], None, false).unwrap();
        assert_eq!(r.singles.added, 2);
        assert_eq!(r.sealed.added, 0);
    }
}
