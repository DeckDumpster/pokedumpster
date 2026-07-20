//! Import dead-letter queue — the persistent "unresolved" backlog
//! (pokedumpster-oq3i.5).
//!
//! When an import row doesn't resolve to a catalog item, its *parsed* form is
//! parked here as JSON (`raw`). The user later matches it to a printing or
//! sealed product manually; resolving replays `raw` against the picked catalog
//! id — so per-row metadata (condition, price, acquired date, tags, quantity)
//! is preserved, not thrown away — writes the copy via [`crate::collection`] /
//! [`crate::sealed`], and marks the queue row resolved.
//!
//! Deliberately *not* the `batches` table: batches group *committed* copies,
//! and an unresolved row has no collection row yet and a different lifecycle.

use rusqlite::{Connection, OptionalExtension, params};

use pkdump_core::import::{ParsedRow, ParsedSealedRow};

use crate::batches::{self, NewBatch};
use crate::collection::{self, CopyEdit, NewCopy};
use crate::error::{DbError, Result};
use crate::import::{ResolutionReport, tags_json};
use crate::sealed::{self, NewSealed, SealedEntry};
use crate::sealed_import::SealedResolutionReport;

/// One open row of the dead-letter queue, as shown on `/ingest/unresolved`.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct UnresolvedRow {
    #[ts(type = "number")]
    pub id: i64,
    /// `single` | `sealed`.
    pub kind: String,
    /// The import source that parked it (`csv_collectr`, `csv_manabox`, …).
    pub source: String,
    #[ts(type = "number | null")]
    pub batch_id: Option<i64>,
    #[ts(type = "number | null")]
    pub source_line: Option<i64>,
    pub set_hint: Option<String>,
    /// Collector number (singles only).
    pub number: Option<String>,
    /// Display hint — the card or product name as written in the import.
    pub name: Option<String>,
    /// Variant code (singles only).
    pub variant: Option<String>,
    /// Unit count (sealed only).
    #[ts(type = "number | null")]
    pub quantity: Option<i64>,
    /// The resolver's human-readable reason this row didn't match.
    pub reason: String,
    pub parked_at: String,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Park one unresolved import row. `raw` is the JSON serialization of the
/// row's [`ParsedRow`]/[`ParsedSealedRow`] — the lossless replay source.
/// Returns the new queue-row id.
#[allow(clippy::too_many_arguments)]
pub fn park(
    conn: &Connection,
    kind: &str,
    source: &str,
    batch_id: Option<i64>,
    source_line: Option<i64>,
    raw: &str,
    set_hint: Option<&str>,
    number: Option<&str>,
    name: Option<&str>,
    variant: Option<&str>,
    quantity: Option<i64>,
    reason: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO import_unresolved \
           (kind, source, batch_id, source_line, raw, set_hint, number, name, \
            variant, quantity, reason, status, parked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'open', ?12)",
        params![
            kind,
            source,
            batch_id,
            source_line,
            raw,
            set_hint,
            number,
            name,
            variant,
            quantity,
            reason,
            now(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Park the leftover unmatched rows of an import's resolution — the singles
/// half and the sealed half — into the queue under `batch_id`. Each parked row
/// carries the *parsed* source (from `singles` / `sealed`) as its replay `raw`,
/// so a single CSV line expanded to N physical copies parks N rows. Returns the
/// number of rows parked. (pokedumpster-oq3i.5)
pub fn park_report(
    conn: &Connection,
    source: &str,
    batch_id: Option<i64>,
    singles: &[ParsedRow],
    singles_report: &ResolutionReport,
    sealed: &[ParsedSealedRow],
    sealed_report: &SealedResolutionReport,
) -> Result<u32> {
    let mut parked = 0u32;

    // Singles: map each unmatched source_line to its reason, then park every
    // parsed copy on those lines (rows on a line are identical, so all match or
    // all miss together — one queue row per physical copy).
    for row in singles {
        let line = row.source_line as u32;
        let Some(reason) = singles_report
            .unmatched
            .iter()
            .find(|u| u.source_line == line)
            .map(|u| u.reason.clone())
        else {
            continue;
        };
        let raw = serde_json::to_string(row)?;
        park(
            conn,
            "single",
            source,
            batch_id,
            Some(row.source_line as i64),
            &raw,
            Some(&row.set_hint),
            Some(&row.number),
            row.name.as_deref(),
            Some(&row.variant),
            None,
            &reason,
        )?;
        parked += 1;
    }

    // Sealed: one parsed row per line (quantity kept, never expanded).
    for row in sealed {
        let line = row.source_line as u32;
        let Some(reason) = sealed_report
            .unmatched
            .iter()
            .find(|u| u.source_line == line)
            .map(|u| u.reason.clone())
        else {
            continue;
        };
        let raw = serde_json::to_string(row)?;
        park(
            conn,
            "sealed",
            source,
            batch_id,
            Some(row.source_line as i64),
            &raw,
            Some(&row.set_hint),
            None,
            Some(&row.name),
            None,
            Some(i64::from(row.quantity)),
            &reason,
        )?;
        parked += 1;
    }

    Ok(parked)
}

/// List every open queue row, newest import first.
pub fn list_open(conn: &Connection) -> Result<Vec<UnresolvedRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, source, batch_id, source_line, set_hint, number, \
                name, variant, quantity, reason, parked_at \
         FROM import_unresolved WHERE status = 'open' \
         ORDER BY parked_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(UnresolvedRow {
            id: r.get(0)?,
            kind: r.get(1)?,
            source: r.get(2)?,
            batch_id: r.get(3)?,
            source_line: r.get(4)?,
            set_hint: r.get(5)?,
            number: r.get(6)?,
            name: r.get(7)?,
            variant: r.get(8)?,
            quantity: r.get(9)?,
            reason: r.get(10)?,
            parked_at: r.get(11)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The stored fields of a queue row needed to replay it.
struct Parked {
    kind: String,
    source: String,
    batch_id: Option<i64>,
    raw: String,
    status: String,
}

fn load(conn: &Connection, id: i64) -> Result<Parked> {
    conn.prepare("SELECT kind, source, batch_id, raw, status FROM import_unresolved WHERE id = ?1")?
        .query_row([id], |r| {
            Ok(Parked {
                kind: r.get(0)?,
                source: r.get(1)?,
                batch_id: r.get(2)?,
                raw: r.get(3)?,
                status: r.get(4)?,
            })
        })
        .optional()?
        .ok_or_else(|| DbError::NotFound(format!("unresolved import row {id}")))
}

/// Guard: the row must exist, still be open, and be of the expected `kind`.
fn expect_open(row: &Parked, id: i64, kind: &str) -> Result<()> {
    if row.status != "open" {
        return Err(DbError::Conflict(format!(
            "unresolved import row {id} is already {}",
            row.status
        )));
    }
    if row.kind != kind {
        return Err(DbError::Conflict(format!(
            "unresolved import row {id} is a {} row, not {kind}",
            row.kind
        )));
    }
    Ok(())
}

/// Resolve a parked single to a chosen `printing_id`: replay the stored
/// [`ParsedRow`] into a new `collection` copy (preserving condition, price,
/// acquired date, language, tags), then mark the queue row resolved. Returns
/// the created collection row.
pub fn resolve_single(
    conn: &mut Connection,
    id: i64,
    printing_id: &str,
) -> Result<collection::CollectionRow> {
    let row = load(conn, id)?;
    expect_open(&row, id, "single")?;
    let parsed: ParsedRow = serde_json::from_str(&row.raw)?;

    // Into the import's original batch, or a fresh manual-resolution batch.
    let batch_id = match row.batch_id {
        Some(b) => b,
        None => batches::create(
            conn,
            &NewBatch {
                batch_type: "manual_resolution".to_string(),
                name: Some("Manual import resolution".to_string()),
                ..Default::default()
            },
        )?,
    };

    let copy_id = collection::add(
        conn,
        &NewCopy {
            printing_id: printing_id.to_string(),
            condition: Some(parsed.condition.clone()),
            language: Some(parsed.language.clone()),
            purchase_price: parsed.purchase_price,
            acquired_at: parsed.acquired_at.clone(),
            source: row.source.clone(),
            batch_id: Some(batch_id),
            ..Default::default()
        },
    )?;
    if !parsed.tags.is_empty() {
        collection::update(
            conn,
            copy_id,
            &CopyEdit {
                tags: Some(tags_json(&parsed.tags)),
                ..Default::default()
            },
        )?;
    }

    conn.execute(
        "UPDATE import_unresolved \
         SET status = 'resolved', resolved_printing_id = ?1, \
             resolved_collection_id = ?2, resolved_at = ?3 \
         WHERE id = ?4",
        params![printing_id, copy_id, now(), id],
    )?;

    collection::get_row(conn, copy_id)?.ok_or_else(|| {
        DbError::NotFound(format!("collection row {copy_id} vanished after resolve"))
    })
}

/// Resolve a parked sealed row to a chosen `product_id`: replay the stored
/// [`ParsedSealedRow`] into a new `sealed_collection` row (quantity, condition,
/// price, date, notes preserved), then mark the queue row resolved. Returns the
/// created sealed entry.
pub fn resolve_sealed(conn: &mut Connection, id: i64, product_id: i64) -> Result<SealedEntry> {
    let row = load(conn, id)?;
    expect_open(&row, id, "sealed")?;
    let parsed: ParsedSealedRow = serde_json::from_str(&row.raw)?;

    let sealed_id = sealed::add(
        conn,
        &NewSealed {
            product_id,
            quantity: Some(i64::from(parsed.quantity)),
            condition: Some(parsed.condition.clone()),
            purchase_price: parsed.purchase_price,
            purchase_date: parsed.purchase_date.clone(),
            source: Some(row.source.clone()),
            seller_name: None,
            notes: parsed.notes.clone(),
        },
    )?;

    conn.execute(
        "UPDATE import_unresolved \
         SET status = 'resolved', resolved_product_id = ?1, \
             resolved_sealed_id = ?2, resolved_at = ?3 \
         WHERE id = ?4",
        params![product_id, sealed_id, now(), id],
    )?;

    sealed::get(conn, sealed_id)?
        .ok_or_else(|| DbError::NotFound(format!("sealed row {sealed_id} vanished after resolve")))
}

/// Dismiss a parked row (genuinely-nonexistent junk): mark it `dismissed`
/// without writing a copy. Errors if there is no open row with this id.
pub fn dismiss(conn: &Connection, id: i64) -> Result<()> {
    let n = conn.execute(
        "UPDATE import_unresolved SET status = 'dismissed', resolved_at = ?1 \
         WHERE id = ?2 AND status = 'open'",
        params![now(), id],
    )?;
    if n == 0 {
        return Err(DbError::NotFound(format!(
            "no open unresolved import row {id}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{self, ImportFormat};
    use crate::{connect_user, open_shared};

    /// A catalog with one single-card printing and one sealed product, plus
    /// the misc catch-all so unmatched rows are easy to produce.
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
            c.execute(
                "INSERT INTO sealed_products (product_id, set_code, name, category, fetched_at) \
                 VALUES (7001, 'sv3pt5', '151 Elite Trainer Box', 'elite_trainer_box', '2026-02-28')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    fn single(number: &str, variant: &str) -> ParsedRow {
        ParsedRow {
            source_line: 2,
            set_hint: "MEW".into(),
            set_name: Some("151".into()),
            name: Some("Charizard ex".into()),
            number: number.into(),
            variant: variant.into(),
            condition: "Lightly Played".into(),
            language: "English".into(),
            purchase_price: Some(4.25),
            acquired_at: Some("2026-04-14T00:00:00Z".into()),
            tags: vec!["misprint".into()],
        }
    }

    fn sealed_row(name: &str, qty: u32) -> ParsedSealedRow {
        ParsedSealedRow {
            source_line: 3,
            name: name.into(),
            set_hint: "151".into(),
            set_name: Some("151".into()),
            category_hint: None,
            quantity: qty,
            condition: "Near Mint".into(),
            purchase_price: Some(49.99),
            purchase_date: Some("2026-02-28".into()),
            notes: Some("shrink-wrapped".into()),
        }
    }

    #[test]
    fn park_list_then_resolve_single_replays_metadata() {
        let (_d, mut conn) = db();
        // Park a single that couldn't resolve (variant 'holo' unavailable).
        let raw = serde_json::to_string(&single("6", "holo")).unwrap();
        park(
            &conn,
            "single",
            "csv_manabox",
            None,
            Some(2),
            &raw,
            Some("MEW"),
            Some("6"),
            Some("Charizard ex"),
            Some("holo"),
            None,
            "variant 'holo' not available",
        )
        .unwrap();

        let open = list_open(&conn).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, "single");
        assert_eq!(open[0].name.as_deref(), Some("Charizard ex"));

        // Resolve to the real 'normal' printing — the replayed copy keeps the
        // parked condition/price/acquired/tags.
        let created = resolve_single(&mut conn, open[0].id, "sv3pt5-6-normal").unwrap();
        assert_eq!(created.printing_id, "sv3pt5-6-normal");
        assert_eq!(created.condition, "Lightly Played");
        assert_eq!(created.purchase_price, Some(4.25));
        assert_eq!(created.acquired_at, "2026-04-14T00:00:00Z");
        assert_eq!(created.source, "csv_manabox");

        // The copy exists, and the queue is empty again.
        assert!(list_open(&conn).unwrap().is_empty());
        assert_eq!(collection::list_rows(&conn).unwrap().len(), 1);
        // The tag survived the replay (stored as a JSON array string).
        let entry = collection::get(&conn, created.id).unwrap().unwrap();
        assert!(
            entry.tags.as_deref().unwrap_or("").contains("misprint"),
            "{:?}",
            entry.tags
        );
    }

    #[test]
    fn resolve_sealed_replays_quantity_and_metadata() {
        let (_d, mut conn) = db();
        let raw = serde_json::to_string(&sealed_row("Elite Trainer Box", 3)).unwrap();
        park(
            &conn,
            "sealed",
            "csv_collectr",
            None,
            Some(3),
            &raw,
            Some("151"),
            None,
            Some("Elite Trainer Box"),
            None,
            Some(3),
            "ambiguous product",
        )
        .unwrap();

        let open = list_open(&conn).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].kind, "sealed");
        assert_eq!(open[0].quantity, Some(3));

        let entry = resolve_sealed(&mut conn, open[0].id, 7001).unwrap();
        assert_eq!(entry.product_id, 7001);
        assert_eq!(entry.quantity, 3); // preserved, not expanded
        assert_eq!(entry.purchase_price, Some(49.99));
        assert_eq!(entry.notes.as_deref(), Some("shrink-wrapped"));
        assert_eq!(entry.source.as_deref(), Some("csv_collectr"));

        assert!(list_open(&conn).unwrap().is_empty());
        assert_eq!(sealed::list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn dismiss_drops_row_without_writing_a_copy() {
        let (_d, conn) = db();
        let raw = serde_json::to_string(&single("999", "normal")).unwrap();
        let id = park(
            &conn,
            "single",
            "csv_manabox",
            None,
            Some(2),
            &raw,
            Some("MEW"),
            Some("999"),
            Some("Ghost Card"),
            Some("normal"),
            None,
            "card #999 not found",
        )
        .unwrap();

        dismiss(&conn, id).unwrap();
        assert!(list_open(&conn).unwrap().is_empty());
        assert!(collection::list_rows(&conn).unwrap().is_empty());
        // Dismissing an already-closed row errors (no silent no-op).
        assert!(dismiss(&conn, id).is_err());
    }

    #[test]
    fn resolve_rejects_wrong_kind() {
        let (_d, mut conn) = db();
        let raw = serde_json::to_string(&single("6", "holo")).unwrap();
        let id = park(
            &conn,
            "single",
            "csv_manabox",
            None,
            Some(2),
            &raw,
            Some("MEW"),
            Some("6"),
            Some("Charizard ex"),
            Some("holo"),
            None,
            "variant holo",
        )
        .unwrap();
        // A single row can't be resolved as sealed.
        assert!(resolve_sealed(&mut conn, id, 7001).is_err());
    }

    #[test]
    fn park_unmatched_on_selected_commit_parks_the_misses() {
        // A ManaBox CSV: line 2 resolves (normal), line 3 misses (holo has no
        // printing), line 4 misses (unknown set). Commit line 2 with
        // park_unmatched=true — the two misses land in the queue.
        let (_d, mut conn) = db();
        const CSV: &str = "\
Set code,Set name,Collector number,Foil,Quantity,Condition,Language,Purchase price
MEW,151,6,normal,1,near_mint,en,1.50
MEW,151,6,foil,1,near_mint,en,5.00
NOPE,Mystery,1,normal,1,near_mint,en,";

        let result = import::commit_selected(
            &mut conn,
            ImportFormat::Manabox,
            CSV,
            &[2],
            Some("import.csv"),
            true,
        )
        .unwrap();
        assert_eq!(result.added, 1);

        let open = list_open(&conn).unwrap();
        assert_eq!(open.len(), 2, "{open:?}");
        assert!(open.iter().all(|r| r.kind == "single"));
        assert!(open.iter().all(|r| r.batch_id == Some(result.batch_id)));
        assert!(open.iter().all(|r| r.source == "csv_manabox"));
    }
}
