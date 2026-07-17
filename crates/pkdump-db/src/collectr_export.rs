//! Collectr-compatible CSV export (PLAN.md §9). Emits the collection in the
//! exact 16-column shape Collectr (getcollectr.com) produces, so a migration
//! back the other way re-imports cleanly there.
//!
//! Honoring the garden wall, singles and sealed are **separate exports** —
//! two files, never one — even though Collectr itself keeps both in one file
//! distinguished by portfolio. Each file is a valid Collectr CSV on its own:
//! the singles file lands in a `Main` portfolio, the sealed file in a
//! `Sealed Pokemon TCG` portfolio. Both also round-trip through this app's
//! own Collectr importer.
//!
//! Only `owned` rows are exported — a migration should carry what you have,
//! not cards you have already sold or given away.

use rusqlite::Connection;

use crate::error::{DbError, Result};

/// Collectr's column order. Kept verbatim so the file is indistinguishable
/// from a native Collectr export.
const HEADER: &[&str] = &[
    "Portfolio Name",
    "Category",
    "Set",
    "Product Name",
    "Card Number",
    "Rarity",
    "Variance",
    "Grade",
    "Card Condition",
    "Average Cost Paid",
    "Quantity",
    "Market Price",
    "Price Override",
    "Watchlist",
    "Date Added",
    "Notes",
];

/// Export owned single cards as a Collectr-shaped CSV (portfolio `Main`).
/// One row per physical copy (`Quantity` always 1 — no aggregation).
pub fn collectr_singles_csv(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT s.name, s.printed_total, cd.name, cd.number, cd.rarity, \
                p.variant, c.condition, c.purchase_price, c.acquired_at, \
                c.graded, c.grade_company, c.grade_value, c.notes \
         FROM collection c \
           JOIN printings p ON c.printing_id = p.printing_id \
           JOIN cards cd ON p.card_id = cd.card_id \
           JOIN sets s ON cd.set_code = s.set_code \
         WHERE c.status = 'owned' \
         ORDER BY s.set_sort_order, cd.number_sortable, p.variant, c.id",
    )?;

    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(HEADER).map_err(csv_err)?;

    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let set_name: String = r.get(0)?;
        let printed_total: Option<i64> = r.get(1)?;
        let card_name: String = r.get(2)?;
        let number: String = r.get(3)?;
        let rarity: Option<String> = r.get(4)?;
        let variant: String = r.get(5)?;
        let condition: String = r.get(6)?;
        let purchase_price: Option<f64> = r.get(7)?;
        let acquired_at: Option<String> = r.get(8)?;
        let graded: i64 = r.get(9)?;
        let grade_company: Option<String> = r.get(10)?;
        let grade_value: Option<f64> = r.get(11)?;
        let notes: Option<String> = r.get(12)?;

        writer
            .write_record([
                "Main",
                "Pokemon",
                &set_name,
                &card_name,
                &card_number(&number, printed_total),
                &rarity.unwrap_or_default(),
                variant_to_collectr(&variant),
                &grade(graded, grade_company.as_deref(), grade_value),
                &condition,
                &money(purchase_price),
                "1",
                "", // Market Price — Collectr recomputes on import.
                "0",
                "false",
                &date_only(acquired_at.as_deref()),
                &notes.unwrap_or_default(),
            ])
            .map_err(csv_err)?;
    }
    finish(writer)
}

/// Export owned sealed products as a Collectr-shaped CSV (portfolio
/// `Sealed Pokemon TCG`). Sealed keeps its `Quantity` — it is not expanded.
pub fn collectr_sealed_csv(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT s.name, sp.name, sc.quantity, sc.condition, \
                sc.purchase_price, sc.purchase_date, sc.notes \
         FROM sealed_collection sc \
           JOIN sealed_products sp ON sc.product_id = sp.product_id \
           LEFT JOIN sets s ON sp.set_code = s.set_code \
         WHERE sc.status = 'owned' \
         ORDER BY s.set_sort_order, sp.name, sc.id",
    )?;

    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(HEADER).map_err(csv_err)?;

    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let set_name: Option<String> = r.get(0)?;
        let product_name: String = r.get(1)?;
        let quantity: i64 = r.get(2)?;
        let condition: Option<String> = r.get(3)?;
        let purchase_price: Option<f64> = r.get(4)?;
        let purchase_date: Option<String> = r.get(5)?;
        let notes: Option<String> = r.get(6)?;

        writer
            .write_record([
                "Sealed Pokemon TCG",
                "Pokemon",
                &set_name.unwrap_or_default(),
                &product_name,
                "", // Card Number — blank marks a sealed product.
                "", // Rarity — sealed products carry none.
                "Normal",
                "Ungraded",
                condition.as_deref().unwrap_or("Near Mint"),
                &money(purchase_price),
                &quantity.to_string(),
                "",
                "0",
                "false",
                &date_only(purchase_date.as_deref()),
                &notes.unwrap_or_default(),
            ])
            .map_err(csv_err)?;
    }
    finish(writer)
}

/// `number` plus `/printed_total` when the set total is known — Collectr's
/// `221/217` shape. Bare number otherwise.
fn card_number(number: &str, printed_total: Option<i64>) -> String {
    match printed_total {
        Some(t) => format!("{number}/{t}"),
        None => number.to_string(),
    }
}

/// PokeDumpster variant code → Collectr `Variance`. The inverse of the
/// importer's foil mapping; uncommon holo variants fold to `Holofoil`.
fn variant_to_collectr(variant: &str) -> &'static str {
    match variant {
        "normal" => "Normal",
        "reverse_holo" => "Reverse Holofoil",
        _ => "Holofoil",
    }
}

/// A Collectr `Grade` cell: `Ungraded`, or e.g. `PSA 10`.
fn grade(graded: i64, company: Option<&str>, value: Option<f64>) -> String {
    if graded == 0 {
        return "Ungraded".to_string();
    }
    match (company, value) {
        (Some(c), Some(v)) => format!("{c} {}", trim_num(v)),
        (Some(c), None) => c.to_string(),
        _ => "Graded".to_string(),
    }
}

/// A price cell: two decimals, or empty when unknown.
fn money(price: Option<f64>) -> String {
    price.map(|p| format!("{p:.2}")).unwrap_or_default()
}

/// The `YYYY-MM-DD` prefix of a stored timestamp (Collectr writes dates,
/// not timestamps).
fn date_only(ts: Option<&str>) -> String {
    ts.map(|t| t.chars().take(10).collect()).unwrap_or_default()
}

/// Format a grade value without a trailing `.0` (`10.0` → `10`, `9.5` stays).
fn trim_num(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

fn finish(writer: csv::Writer<Vec<u8>>) -> Result<String> {
    let bytes = writer
        .into_inner()
        .map_err(|e| DbError::Import(format!("CSV finalize failed: {e}")))?;
    String::from_utf8(bytes).map_err(|e| DbError::Import(format!("CSV not UTF-8: {e}")))
}

fn csv_err(e: csv::Error) -> DbError {
    DbError::Import(format!("CSV write failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sealed::{self, NewSealed};
    use crate::{collection::NewCopy, connect_user, open_shared};

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, ptcgo_code, name, series, printed_total, set_sort_order) \
                 VALUES ('sv3pt5', 'MEW', '151', 'SV', 165, 1)",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                 VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex', 'Double Rare')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-6-holo', 'sv3pt5-6', 'holo')",
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

    #[test]
    fn singles_export_is_collectr_shaped() {
        let (_d, mut conn) = db();
        crate::collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-6-holo".into(),
                condition: Some("Near Mint".into()),
                purchase_price: Some(4.5),
                acquired_at: Some("2026-04-14T00:00:00Z".into()),
                source: "test".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let csv = collectr_singles_csv(&conn).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].starts_with("Portfolio Name,Category,Set,Product Name,Card Number"));
        let row = lines[1];
        assert!(row.starts_with(
            "Main,Pokemon,151,Charizard ex,6/165,Double Rare,Holofoil,Ungraded,Near Mint,4.50,1"
        ));
        assert!(row.contains("2026-04-14"));
    }

    #[test]
    fn sealed_export_keeps_quantity_and_marks_blank_number() {
        let (_d, conn) = db();
        sealed::add(
            &conn,
            &NewSealed {
                product_id: 7001,
                quantity: Some(3),
                purchase_price: Some(49.99),
                purchase_date: Some("2026-02-28".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let csv = collectr_sealed_csv(&conn).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        let row = lines[1];
        // Portfolio, blank Card Number + Rarity, quantity preserved.
        assert!(
            row.starts_with(
                "Sealed Pokemon TCG,Pokemon,151,151 Elite Trainer Box,,,Normal,Ungraded"
            )
        );
        assert!(row.contains(",3,")); // quantity 3, not expanded
        assert!(row.contains("2026-02-28"));
    }

    #[test]
    fn disposed_rows_are_not_exported() {
        let (_d, mut conn) = db();
        let id = crate::collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-6-holo".into(),
                source: "test".into(),
                ..Default::default()
            },
        )
        .unwrap();
        crate::collection::set_status(&mut conn, id, "sold", None).unwrap();

        let csv = collectr_singles_csv(&conn).unwrap();
        assert_eq!(csv.lines().count(), 1); // header only
    }

    /// The garden-wall guarantee, end to end: a Collectr file imports into
    /// the right tables, exports back out as two Collectr files, and those
    /// files re-parse to exactly the same singles/sealed split — cards never
    /// leak into the sealed file, sealed never leaks into the cards file.
    #[test]
    fn collectr_round_trip_holds_the_garden_wall() {
        use pkdump_core::import::collectr;

        let (_d, mut conn) = db();
        let csv = "\
Portfolio Name,Category,Set,Product Name,Card Number,Rarity,Variance,Grade,Card Condition,Average Cost Paid,Quantity,Market Price (As of 2026-07-17),Price Override,Watchlist,Date Added,Notes
Main,Pokemon,151,Charizard ex,6/165,Double Rare,Holofoil,Ungraded,Near Mint,4.50,2,5.00,0,false,2026-04-14,
Sealed Pokemon TCG,Pokemon,151,151 Elite Trainer Box,,,Normal,Ungraded,Near Mint,49.99,3,60.00,0,false,2026-02-28,
Main,Lorcana,Foo,Bar,1,Promo,Holofoil,Ungraded,Near Mint,0,1,1.00,0,false,2026-01-01,";

        // Import: two card copies, one sealed row (qty kept), Lorcana skipped.
        let imported = crate::import::commit_collectr(&mut conn, csv, Some("collectr")).unwrap();
        assert_eq!(imported.singles.added, 2);
        assert_eq!(imported.sealed.added, 1);
        assert_eq!(imported.skipped, 1);

        // Export each half back out as its own Collectr file.
        let singles_csv = collectr_singles_csv(&conn).unwrap();
        let sealed_csv = collectr_sealed_csv(&conn).unwrap();

        // The cards file re-parses to only singles; re-resolves cleanly.
        let re_singles = collectr::parse(&singles_csv).unwrap();
        assert_eq!(re_singles.singles.len(), 2);
        assert!(
            re_singles.sealed.is_empty(),
            "a card leaked into the sealed stream"
        );
        let re_singles_report = crate::import::resolve(&conn, &re_singles.singles).unwrap();
        assert_eq!(re_singles_report.matched.len(), 2);
        assert!(re_singles_report.unmatched.is_empty());

        // The sealed file re-parses to only sealed; quantity survives.
        let re_sealed = collectr::parse(&sealed_csv).unwrap();
        assert_eq!(re_sealed.sealed.len(), 1);
        assert!(
            re_sealed.singles.is_empty(),
            "a sealed product leaked into the card stream"
        );
        assert_eq!(re_sealed.sealed[0].quantity, 3);
        let re_sealed_report =
            crate::sealed_import::resolve_sealed(&conn, &re_sealed.sealed).unwrap();
        assert_eq!(re_sealed_report.matched.len(), 1);
        assert_eq!(re_sealed_report.matched[0].product_id, 7001);
    }
}
