//! The `variants` shared-catalog table: display metadata for every
//! `printings.variant` code. `data/variants.json` is the canonical
//! authoring source; `reconcile()` re-applies the file on every
//! `pkdump setup` run and synthesizes rows for any set-specific
//! stamp codes that ingest produces (`stamp_<set_keyword>`) so the
//! FK on `printings.variant` is always satisfied.
//!
//! With this table in place, the frontend's variantLabel/variantRank/
//! variantColor/variantTag heuristics collapse into table lookups —
//! the data model owns the display contract.

use rusqlite::{Connection, params};

use crate::error::Result;

const VARIANTS_SEED: &str = include_str!("../../../data/variants.json");

/// One row of the variants table. Mirrors the schema 1:1.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Variant {
    pub code: String,
    pub label: String,
    pub short: String,
    #[ts(type = "number")]
    pub rank: i64,
    pub color: String,
}

/// Re-seed `variants` from `data/variants.json` and synthesize rows for
/// any `printings.variant` codes the JSON doesn't cover (set-specific
/// stamps discovered at ingest time). Called from `pkdump setup` before
/// variant expansion, so every variant code the expander emits is known
/// to the FK by the time we INSERT.
pub fn reconcile(conn: &mut Connection) -> Result<usize> {
    let seed: Vec<Variant> = serde_json::from_str(VARIANTS_SEED)?;
    let tx = conn.transaction()?;
    for v in &seed {
        upsert(&tx, v)?;
    }
    // Catch set-specific stamp codes that already exist in printings but
    // aren't in the seed. Synthesize a human label from the suffix.
    let unknown: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT p.variant FROM printings p \
             WHERE p.variant NOT IN (SELECT code FROM variants)",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for code in &unknown {
        upsert(&tx, &synthesize(code))?;
    }
    tx.commit()?;
    Ok(seed.len() + unknown.len())
}

/// Ensure a variant code has a row before something tries to FK to it.
/// Used by ingest right before inserting a printing with a freshly-
/// discovered stamp code.
pub fn ensure_code(conn: &Connection, code: &str) -> Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT count(*) FROM variants WHERE code = ?1",
        [code],
        |r| r.get(0),
    )?;
    if exists == 0 {
        upsert(conn, &synthesize(code))?;
    }
    Ok(())
}

/// Read the full variants table — backs the `/api/variants` endpoint.
pub fn list_all(conn: &Connection) -> Result<Vec<Variant>> {
    let mut stmt =
        conn.prepare("SELECT code, label, short, rank, color FROM variants ORDER BY rank, code")?;
    let rows = stmt.query_map([], |r| {
        Ok(Variant {
            code: r.get(0)?,
            label: r.get(1)?,
            short: r.get(2)?,
            rank: r.get(3)?,
            color: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn upsert(conn: &Connection, v: &Variant) -> Result<()> {
    conn.execute(
        "INSERT INTO variants (code, label, short, rank, color) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(code) DO UPDATE SET \
           label = excluded.label, short = excluded.short, \
           rank = excluded.rank, color = excluded.color",
        params![v.code, v.label, v.short, v.rank, v.color],
    )?;
    Ok(())
}

/// Synthesize a Variant for a code we don't have in the seed — almost
/// always a set-specific `stamp_<keyword>` produced by overlay ingest
/// when a new expansion ships a stamped product. Format:
///   `stamp_black_bolt`         → "Black Bolt Stamp"
///   `stamp_sdcc_2007_staff`    → "SDCC 2007 Staff Stamp"
///   `stamp_buildbattle`        → "Buildbattle Stamp" (would be in seed)
fn synthesize(code: &str) -> Variant {
    let label = if let Some(suffix) = code.strip_prefix("stamp_") {
        let parts: Vec<String> = suffix.split('_').map(title_case_word).collect();
        format!("{} Stamp", parts.join(" "))
    } else {
        // Generic fallback — unknown family, just title-case the code.
        code.split('_')
            .map(title_case_word)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let short = if code.starts_with("stamp_") {
        "STAMP".to_string()
    } else {
        "?".to_string()
    };
    Variant {
        code: code.to_string(),
        label,
        short,
        rank: 4,
        color: "#b88cc0".to_string(),
    }
}

fn title_case_word(w: &str) -> String {
    // SDCC stays uppercase — short acronym tokens shouldn't be downcased.
    if w.chars().all(|c| c.is_ascii_uppercase()) && w.len() <= 4 && !w.is_empty() {
        return w.to_string();
    }
    let lower = w.to_ascii_lowercase();
    match lower.as_str() {
        "sdcc" | "e3" => lower.to_ascii_uppercase(),
        _ => {
            let mut chars = lower.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::open_shared(&dir.path().join("shared.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn seed_applies_idempotently() {
        let (_d, mut conn) = fresh();
        let n1 = reconcile(&mut conn).unwrap();
        let n2 = reconcile(&mut conn).unwrap();
        assert_eq!(n1, n2, "row count is stable on re-run");
        let label: String = conn
            .query_row(
                "SELECT label FROM variants WHERE code = 'pokeball_rh'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(label, "Poké Ball Reverse Holo");
    }

    #[test]
    fn synthesize_makes_stamp_labels_readable() {
        assert_eq!(synthesize("stamp_black_bolt").label, "Black Bolt Stamp");
        assert_eq!(
            synthesize("stamp_sdcc_2007_staff").label,
            "SDCC 2007 Staff Stamp"
        );
        assert_eq!(
            synthesize("stamp_prismatic_evolutions").label,
            "Prismatic Evolutions Stamp"
        );
        assert_eq!(synthesize("stamp_black_bolt").short, "STAMP");
    }

    #[test]
    fn ensure_code_inserts_unknown_stamp() {
        let (_d, mut conn) = fresh();
        reconcile(&mut conn).unwrap();
        ensure_code(&conn, "stamp_black_bolt").unwrap();
        let label: String = conn
            .query_row(
                "SELECT label FROM variants WHERE code = 'stamp_black_bolt'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(label, "Black Bolt Stamp");
    }

    #[test]
    fn list_all_orders_by_rank() {
        let (_d, mut conn) = fresh();
        reconcile(&mut conn).unwrap();
        let rows = list_all(&conn).unwrap();
        let normal_idx = rows.iter().position(|v| v.code == "normal").unwrap();
        let pokeball_idx = rows.iter().position(|v| v.code == "pokeball_rh").unwrap();
        assert!(
            normal_idx < pokeball_idx,
            "rank-0 normal should sort before rank-3 pokeball_rh"
        );
    }
}
