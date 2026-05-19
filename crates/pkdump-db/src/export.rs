//! CSV export (PLAN.md §9) — the collection rendered as a ManaBox-shaped
//! CSV that re-imports cleanly through `import::manabox`.

use rusqlite::Connection;

use crate::error::Result;

/// The ManaBox export header, in ManaBox's column order.
const HEADER: &[&str] = &[
    "Set code",
    "Set name",
    "Collector number",
    "Foil",
    "Rarity",
    "Quantity",
    "ManaBox ID",
    "Scryfall ID",
    "Purchase price",
    "Misprint",
    "Altered",
    "Condition",
    "Language",
    "Purchase price currency",
];

/// Export the whole collection as a ManaBox-shaped CSV string. One row per
/// physical copy (Quantity is always 1 — no aggregation), with the `Foil`
/// column holding the PokeDumpster variant code so every variant survives
/// a round-trip back through the importer.
pub fn manabox_csv(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT s.set_code, s.name, cd.number, p.variant, cd.rarity, \
                c.purchase_price, c.condition, c.language, c.tags \
         FROM collection c \
           JOIN printings p ON c.printing_id = p.printing_id \
           JOIN cards cd ON p.card_id = cd.card_id \
           JOIN sets s ON cd.set_code = s.set_code \
         ORDER BY s.set_code, cd.number_sortable, p.variant, c.id",
    )?;

    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(HEADER).map_err(csv_err)?;

    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        let set_code: String = r.get(0)?;
        let set_name: String = r.get(1)?;
        let number: String = r.get(2)?;
        let variant: String = r.get(3)?;
        let rarity: Option<String> = r.get(4)?;
        let purchase_price: Option<f64> = r.get(5)?;
        let condition: String = r.get(6)?;
        let language: String = r.get(7)?;
        let tags: Option<String> = r.get(8)?;

        let tags = tags.unwrap_or_default();
        let price = purchase_price
            .map(|p| format!("{p:.2}"))
            .unwrap_or_default();
        let currency = if purchase_price.is_some() { "USD" } else { "" };

        writer
            .write_record([
                &set_code,
                &set_name,
                &number,
                &variant,
                rarity.as_deref().unwrap_or(""),
                "1",
                "",
                "",
                &price,
                bool_str(tags.contains("misprint")),
                bool_str(tags.contains("altered")),
                &condition,
                &language,
                currency,
            ])
            .map_err(csv_err)?;
    }

    let bytes = writer
        .into_inner()
        .map_err(|e| crate::DbError::Import(format!("CSV write error: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| crate::DbError::Import(format!("export produced invalid UTF-8: {e}")))
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

/// A `csv` writer error surfaced as a `DbError`.
fn csv_err(e: csv::Error) -> crate::DbError {
    crate::DbError::Import(format!("CSV write error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{self, NewCopy};
    use crate::{connect_user, open_shared};

    #[test]
    fn exports_a_manabox_shaped_csv() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'SV')",
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
                 VALUES ('sv3pt5-6-normal', 'sv3pt5-6', 'normal')",
                [],
            )
            .unwrap();
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-6-normal".into(),
                source: "manual_id".into(),
                purchase_price: Some(12.5),
                ..Default::default()
            },
        )
        .unwrap();

        let csv = manabox_csv(&conn).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 2); // header + one copy
        assert!(lines[0].starts_with("Set code,Set name,Collector number,Foil"));
        assert!(lines[1].contains("sv3pt5"));
        assert!(lines[1].contains("12.50"));
        assert!(lines[1].contains("USD"));

        // The export round-trips back through the ManaBox importer.
        let parsed = pkdump_core::import::manabox::parse(&csv).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].variant, "normal");
        assert_eq!(parsed[0].purchase_price, Some(12.5));
    }
}
