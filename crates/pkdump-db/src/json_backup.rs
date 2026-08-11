//! Portable JSON export/import of the whole user database (pd-e366ae30).
//!
//! A single versioned envelope holding every row of every user table:
//!
//! ```json
//! {
//!   "format": "pkdump-collection",
//!   "version": 1,
//!   "exported_at": "2026-07-31T04:18:44Z",
//!   "collection": [ { "id": 1, "printing_id": "sv3pt5-6-normal", ... } ],
//!   "binders": [ ... ]
//! }
//! ```
//!
//! Round-trip fidelity is the point: export → import into a fresh database
//! reproduces the same logical state, primary keys included. That makes the
//! envelope a human-inspectable backup, a fixture source, and a way to move a
//! collection between machines without copying SQLite files.
//!
//! The table and column lists are read from the schema at runtime
//! (`sqlite_master` + `PRAGMA table_info`), never hardcoded — the data model
//! is the product, so a table added to `schema_user.sql` is exported and
//! imported without touching this file. Only `main` is walked: the shared
//! catalog is `ATTACH`ed and reproducible from upstream, so it never belongs
//! in a collection backup.

use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, params_from_iter};
use serde_json::{Map, Number, Value};

use crate::error::{DbError, Result};

/// The envelope's `format` discriminator.
pub const FORMAT: &str = "pkdump-collection";

/// The envelope schema version. Bumped only when the envelope's *shape*
/// changes — adding a table does not change the shape.
pub const VERSION: u64 = 1;

const KEY_FORMAT: &str = "format";
const KEY_VERSION: &str = "version";
const KEY_EXPORTED_AT: &str = "exported_at";

/// Top-level keys that carry envelope metadata rather than a table.
const META_KEYS: [&str; 3] = [KEY_FORMAT, KEY_VERSION, KEY_EXPORTED_AT];

/// One envelope row staged for insert: `(column, value)` pairs already
/// validated against the live schema.
type StagedRow = Vec<(String, SqlValue)>;

/// One table's staged rows, with its name.
type StagedTable = (String, Vec<StagedRow>);

/// What to do when the target database already holds rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExisting {
    /// Refuse the import. The default — an envelope import is a whole-database
    /// replace, not a merge.
    Fail,
    /// Wipe every user table first, then load the envelope.
    Replace,
}

/// Rows written per table by an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSummary {
    /// `(table, rows)` for every user table, in schema order — counted after
    /// the load, and since the load empties every table first, that count is
    /// what the import put there. Tables absent from the envelope report
    /// zero, unless the schema's own seed refilled them.
    pub tables: Vec<(String, usize)>,
}

impl ImportSummary {
    /// Total rows inserted across all tables.
    pub fn total(&self) -> usize {
        self.tables.iter().map(|(_, n)| n).sum()
    }
}

/// Every user table in the connection's `main` database, alphabetically.
///
/// Skips SQLite's own internals and nothing else. A table the collection
/// should not carry is dropped by `schema_user.sql` on open, not named here
/// — an exclusion list in the exporter is a second, quieter description of
/// the schema, and it drifts (pd-yj40).
pub fn user_tables(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM main.sqlite_master \
         WHERE type = 'table' \
           AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The column names of `table`, in declaration order.
fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Export the whole user database as a pretty-printed JSON envelope.
pub fn export(conn: &Connection) -> Result<String> {
    serde_json::to_string_pretty(&export_value(conn)?).map_err(DbError::Seed)
}

/// Export the whole user database as a JSON envelope.
pub fn export_value(conn: &Connection) -> Result<Value> {
    let mut envelope = Map::new();
    envelope.insert(KEY_FORMAT.to_string(), Value::String(FORMAT.to_string()));
    envelope.insert(KEY_VERSION.to_string(), Value::from(VERSION));
    envelope.insert(
        KEY_EXPORTED_AT.to_string(),
        Value::String(chrono::Utc::now().to_rfc3339()),
    );

    for table in user_tables(conn)? {
        if META_KEYS.contains(&table.as_str()) {
            return Err(DbError::Import(format!(
                "table '{table}' collides with a reserved envelope key"
            )));
        }
        envelope.insert(table.clone(), Value::Array(table_rows(conn, &table)?));
    }
    Ok(Value::Object(envelope))
}

/// Every row of `table` as a JSON object keyed by column name, in `rowid`
/// order so repeated exports of the same state are byte-identical.
fn table_rows(conn: &Connection, table: &str) -> Result<Vec<Value>> {
    let cols = columns(conn, table)?;
    let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))?;
    let mut rows = stmt.query([])?;

    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut obj = Map::new();
        for (i, col) in cols.iter().enumerate() {
            obj.insert(col.clone(), to_json(row.get_ref(i)?, table, col)?);
        }
        out.push(Value::Object(obj));
    }
    Ok(out)
}

/// A SQLite value as JSON. BLOBs and non-finite REALs have no JSON
/// representation — no user column holds either, so they fail loudly rather
/// than silently losing data.
fn to_json(value: ValueRef<'_>, table: &str, col: &str) -> Result<Value> {
    Ok(match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => Number::from_f64(f).map(Value::Number).ok_or_else(|| {
            DbError::Import(format!("{table}.{col}: {f} has no JSON representation"))
        })?,
        ValueRef::Text(t) => Value::String(
            std::str::from_utf8(t)
                .map_err(|e| DbError::Import(format!("{table}.{col}: invalid UTF-8: {e}")))?
                .to_string(),
        ),
        ValueRef::Blob(_) => {
            return Err(DbError::Import(format!(
                "{table}.{col}: BLOB values are not representable in the JSON export"
            )));
        }
    })
}

/// Load a JSON envelope into the user database, replacing its entire
/// contents.
///
/// The whole load runs in one transaction with `defer_foreign_keys` on, so
/// tables can be written in any order and every foreign key is still checked
/// at COMMIT — a bad envelope leaves the database untouched.
pub fn import(conn: &mut Connection, json: &str, on_existing: OnExisting) -> Result<ImportSummary> {
    let parsed: Value =
        serde_json::from_str(json).map_err(|e| DbError::Import(format!("malformed JSON: {e}")))?;
    let envelope = parsed
        .as_object()
        .ok_or_else(|| DbError::Import("expected a JSON object at the top level".to_string()))?;

    match envelope.get(KEY_FORMAT).and_then(Value::as_str) {
        Some(FORMAT) => {}
        Some(other) => {
            return Err(DbError::Import(format!(
                "not a PokeDumpster collection export: format '{other}' (expected '{FORMAT}')"
            )));
        }
        None => {
            return Err(DbError::Import(format!(
                "missing '{KEY_FORMAT}' — not a PokeDumpster collection export"
            )));
        }
    }
    match envelope.get(KEY_VERSION).and_then(Value::as_u64) {
        Some(VERSION) => {}
        Some(other) => {
            return Err(DbError::Import(format!(
                "unsupported envelope version {other} (this build reads version {VERSION})"
            )));
        }
        None => {
            return Err(DbError::Import(format!("missing '{KEY_VERSION}'")));
        }
    }

    let known = user_tables(conn)?;
    // Validate the whole envelope against the live schema before touching a
    // single row: unknown tables, unknown columns and non-scalar values are
    // rejected up front.
    let mut payload: Vec<StagedTable> = Vec::new();
    for (key, value) in envelope {
        if META_KEYS.contains(&key.as_str()) {
            continue;
        }
        if !known.iter().any(|t| t == key) {
            return Err(DbError::Import(format!(
                "envelope holds unknown table '{key}'"
            )));
        }
        let cols = columns(conn, key)?;
        let rows = value
            .as_array()
            .ok_or_else(|| DbError::Import(format!("table '{key}': expected an array of rows")))?;

        let mut table_payload = Vec::with_capacity(rows.len());
        for (i, row) in rows.iter().enumerate() {
            let row = row
                .as_object()
                .ok_or_else(|| DbError::Import(format!("{key}[{i}]: expected a JSON object")))?;
            let mut cells = Vec::with_capacity(row.len());
            for (col, cell) in row {
                if !cols.iter().any(|c| c == col) {
                    return Err(DbError::Import(format!(
                        "{key}[{i}]: '{col}' is not a column of '{key}'"
                    )));
                }
                cells.push((col.clone(), to_sql(cell, key, col)?));
            }
            table_payload.push(cells);
        }
        payload.push((key.clone(), table_payload));
    }

    if on_existing == OnExisting::Fail
        && let Some(table) = first_beyond_birth_state(conn, &known)?
    {
        return Err(DbError::Conflict(format!(
            "target database is not empty (rows in '{table}'); \
             importing an envelope replaces the whole collection"
        )));
    }

    let tx = conn.transaction()?;
    tx.pragma_update(None, "defer_foreign_keys", true)?;

    // The envelope is the complete state, so tables it omits are emptied too.
    for table in &known {
        tx.execute(&format!("DELETE FROM \"{table}\""), [])?;
    }

    for (table, rows) in &payload {
        for cells in rows {
            let cols: Vec<&str> = cells.iter().map(|(c, _)| c.as_str()).collect();
            let sql = insert_sql(table, &cols);
            let mut stmt = tx.prepare_cached(&sql)?;
            stmt.execute(params_from_iter(cells.iter().map(|(_, v)| v)))?;
        }
    }

    tx.commit()?;

    // The envelope is the complete state, so an envelope written before
    // `conditions` moved into the collection (pd-s4c2) leaves the table
    // emptied — and an empty `conditions` is not a loud failure, it is every
    // multiplier silently defaulting to 1.0. Re-seeding fills only what the
    // envelope did not carry, exactly as the next open would.
    crate::conditions::seed_defaults(conn)?;

    // Counted off the database rather than off the payload. Every table was
    // emptied above, so what each one holds now *is* what this import put
    // there — including the rows the seed just restored, which a
    // payload-derived count would report as zero.
    let mut counts: Vec<(String, usize)> = Vec::with_capacity(known.len());
    for table in &known {
        counts.push((table.clone(), row_count(conn, table)? as usize));
    }

    Ok(ImportSummary { tables: counts })
}

/// `INSERT INTO "t" ("a", "b") VALUES (?1, ?2)`, or a bare-DEFAULTS insert
/// for the degenerate empty-object row.
fn insert_sql(table: &str, cols: &[&str]) -> String {
    if cols.is_empty() {
        return format!("INSERT INTO \"{table}\" DEFAULT VALUES");
    }
    let names: Vec<String> = cols.iter().map(|c| format!("\"{c}\"")).collect();
    let holes: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    format!(
        "INSERT INTO \"{table}\" ({}) VALUES ({})",
        names.join(", "),
        holes.join(", ")
    )
}

/// The first table in `tables` holding more rows than a *brand-new*
/// collection is born with — the question `OnExisting::Fail` is actually
/// asking, which is "would this import destroy something".
///
/// Not "holding at least one row": a fresh collection is not empty. The user
/// schema seeds `conditions` with five defaults on every open (pd-s4c2), so
/// against a zero test every single `pkdump import --json` would refuse a
/// database nobody had put anything into.
///
/// The birth state is measured rather than listed — an empty in-memory
/// collection is built through the same [`crate::init_user_schema`] the real
/// one goes through, and its row counts are the baseline. A seed added to
/// `schema_user.sql` tomorrow is accounted for here without this file
/// learning its name, which is the same reason the exporter walks
/// `sqlite_master` instead of naming tables (pd-yj40).
fn first_beyond_birth_state(conn: &Connection, tables: &[String]) -> Result<Option<String>> {
    let newborn = Connection::open_in_memory()?;
    crate::init_user_schema(&newborn)?;

    for table in tables {
        let n = row_count(conn, table)?;
        // A table the newborn does not have at all is entirely the caller's:
        // any row in it is data this import would destroy.
        let baseline = row_count(&newborn, table).unwrap_or(0);
        if n > baseline {
            return Ok(Some(table.clone()));
        }
    }
    Ok(None)
}

fn row_count(conn: &Connection, table: &str) -> Result<i64> {
    Ok(
        conn.query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
            r.get(0)
        })?,
    )
}

/// A JSON value as a SQLite value. Booleans are accepted as 0/1 for the
/// benefit of hand-written envelopes; the exporter never emits them.
fn to_sql(value: &Value, table: &str, col: &str) -> Result<SqlValue> {
    Ok(match value {
        Value::Null => SqlValue::Null,
        Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        Value::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => SqlValue::Integer(i),
            (None, Some(f)) => SqlValue::Real(f),
            (None, None) => {
                return Err(DbError::Import(format!(
                    "{table}.{col}: {n} is out of range for SQLite"
                )));
            }
        },
        Value::String(s) => SqlValue::Text(s.clone()),
        Value::Array(_) | Value::Object(_) => {
            return Err(DbError::Import(format!(
                "{table}.{col}: nested JSON is not a SQLite value"
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};
    use std::path::Path;

    /// A shared catalog with one card, enough for `connect_user` to attach.
    fn seed_shared(path: &Path) {
        let conn = open_shared(path).unwrap();
        conn.execute(
            "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
             VALUES ('sv3pt5-6', 'sv3pt5', '6', 6, 'Charizard ex', 'Double Rare')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO printings (printing_id, card_id, variant) \
             VALUES ('sv3pt5-6-normal', 'sv3pt5-6', 'normal')",
            [],
        )
        .unwrap();
    }

    /// Populate *every* user table, so the round-trip test actually covers
    /// the whole schema. `every_user_table_is_covered` fails if a new table
    /// is added to `schema_user.sql` without a row here.
    fn seed_user(conn: &Connection) {
        conn.execute_batch(
            "INSERT INTO settings (key, value) VALUES ('theme', 'dark'), ('unicode', 'Pikachu ⚡');

             INSERT INTO binders (id, name, description, color, binder_type, pocket_size,
                                  storage_location, created_at, updated_at)
             VALUES (1, 'Trade Binder', NULL, '#ff0000', 'zip', 9, 'shelf',
                     '2026-07-01T00:00:00+00:00', '2026-07-01T00:00:00+00:00');

             INSERT INTO decks (id, name, description, format, owner, state, sleeve_color,
                                storage_location, notes, created_at, updated_at)
             VALUES (1, 'Charizard ex', NULL, 'standard', 'Ryan', 'built', 'black', 'box',
                     'goes brrr', '2026-07-01T00:00:00+00:00', '2026-07-01T00:00:00+00:00');

             INSERT INTO orders (id, order_number, source, seller_name, order_date, subtotal,
                                 shipping, tax, total, shipping_status, estimated_delivery,
                                 notes, created_at)
             VALUES (1, 'ABC-123', 'tcgplayer', 'Some Seller', '2026-06-30', 10.5, 1.25, 0.87,
                     12.62, 'delivered', NULL, NULL, '2026-07-01T00:00:00+00:00');

             INSERT INTO batches (id, batch_type, name, notes, order_id, binder_id, created_at)
             VALUES (1, 'order_tcg', 'July order', NULL, 1, 1, '2026-07-01T00:00:00+00:00');

             INSERT INTO collection (id, printing_id, condition, language, purchase_price,
                                     sale_price, acquired_at, source, notes, tags, graded,
                                     grade_company, grade_value, grade_cert, status, order_id,
                                     binder_id, deck_id, batch_id)
             VALUES (1, 'sv3pt5-6-normal', 'Near Mint', 'English', 12.5, NULL,
                     '2026-07-01T00:00:00+00:00', 'order_import', 'first one',
                     '[\"misprint\"]', 1, 'PSA', 9.5, 'CERT-1', 'owned', 1, 1, NULL, 1),
                    (2, 'sv3pt5-6-normal', 'Lightly Played', 'Japanese', NULL, 3.0,
                     '2026-07-02T00:00:00+00:00', 'manual_id', NULL, NULL, 0,
                     NULL, NULL, NULL, 'sold', NULL, NULL, 1, NULL);

             INSERT INTO status_log (id, collection_id, from_status, to_status, changed_at, note)
             VALUES (1, 2, 'owned', 'sold', '2026-07-03T00:00:00+00:00', 'eBay');

             INSERT INTO movement_log (id, collection_id, from_binder_id, to_binder_id,
                                       from_deck_id, to_deck_id, changed_at, note)
             VALUES (1, 2, 1, NULL, NULL, 1, '2026-07-03T00:00:00+00:00', NULL);

             INSERT INTO wishlist (id, card_id, printing_id, max_price, priority, notes,
                                   added_at, source, fulfilled_at)
             VALUES (1, 'sv3pt5-6', NULL, 100.0, 2, 'want a clean one',
                     '2026-07-01T00:00:00+00:00', 'manual', NULL);

             INSERT INTO sealed_collection (id, product_id, quantity, condition, purchase_price,
                                            sale_price, purchase_date, source, seller_name,
                                            notes, status, added_at)
             VALUES (1, 4242, 3, 'Near Mint', 99.99, NULL, '2026-06-01', 'lgs', 'Local Shop',
                     NULL, 'owned', '2026-07-01T00:00:00+00:00');

             INSERT INTO manual_prices (id, printing_id, price, observed_at, note, created_at)
             VALUES (1, 'sv3pt5-6-normal', 42.0, '2026-07-01T00:00:00+00:00', NULL,
                     '2026-07-01T00:00:00+00:00');

             INSERT INTO user_printings (printing_id, card_id, variant, description, created_at)
             VALUES ('sv3pt5-6-missing_variant', 'sv3pt5-6', 'missing_variant', 'stamped promo',
                     '2026-07-01T00:00:00+00:00');

             INSERT INTO import_unresolved (id, kind, source, batch_id, source_line, raw,
                                            set_hint, number, name, variant, quantity, reason,
                                            status, resolved_printing_id, resolved_product_id,
                                            resolved_collection_id, resolved_sealed_id,
                                            parked_at, resolved_at)
             VALUES (1, 'single', 'csv_collectr', 1, 17, '{\"name\":\"Mystery\"}', '151', '6',
                     'Mystery', 'normal', NULL, 'no set match', 'open', NULL, NULL, NULL, NULL,
                     '2026-07-01T00:00:00+00:00', NULL);

             INSERT INTO collection_value_snapshot (date, dimension, bucket, market_value,
                                                     cost_basis, card_count)
             VALUES ('2026-07-01', 'all', NULL, 123.45, 100.0, 2),
                    ('2026-07-01', 'set', 'sv3pt5', 123.45, 100.0, 2);",
        )
        .unwrap();
    }

    /// An empty user database with the catalog attached, plus a seeded one.
    struct Fixture {
        _dir: tempfile::TempDir,
        shared: std::path::PathBuf,
        dir: std::path::PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let shared = dir.path().join("shared.sqlite");
            seed_shared(&shared);
            Self {
                shared,
                dir: dir.path().to_path_buf(),
                _dir: dir,
            }
        }

        /// A fresh, empty user database.
        fn user(&self, name: &str) -> Connection {
            connect_user(&self.dir.join(name), &self.shared).unwrap()
        }

        /// A user database populated by [`seed_user`].
        fn seeded(&self, name: &str) -> Connection {
            let conn = self.user(name);
            seed_user(&conn);
            conn
        }
    }

    /// The envelope minus its timestamp — what two exports of the same state
    /// must agree on.
    fn without_timestamp(mut envelope: Value) -> Value {
        envelope
            .as_object_mut()
            .unwrap()
            .remove(KEY_EXPORTED_AT)
            .expect("export must stamp exported_at");
        envelope
    }

    #[test]
    fn round_trip_reproduces_every_table() {
        let fx = Fixture::new();
        let source = fx.seeded("source.sqlite");
        let json = export(&source).unwrap();

        let mut target = fx.user("target.sqlite");
        let summary = import(&mut target, &json, OnExisting::Fail).unwrap();

        // Every table came back with the same number of rows...
        for (table, rows) in &summary.tables {
            let n: i64 = source
                .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(*rows, n as usize, "{table} row count");
        }

        // ...and re-exporting the target reproduces the envelope exactly,
        // primary keys, NULLs, floats and unicode included.
        assert_eq!(
            without_timestamp(export_value(&target).unwrap()),
            without_timestamp(export_value(&source).unwrap())
        );
    }

    #[test]
    fn every_user_table_is_covered() {
        // The round-trip test is only as good as its seed data: assert the
        // fixture leaves no table empty, so a table added to
        // `schema_user.sql` without a seeded row fails here instead of
        // silently going untested.
        let fx = Fixture::new();
        let conn = fx.seeded("coverage.sqlite");
        let envelope = export_value(&conn).unwrap();
        let obj = envelope.as_object().unwrap();

        let tables = user_tables(&conn).unwrap();
        assert!(
            tables.len() >= 14,
            "expected the full user schema: {tables:?}"
        );
        for table in tables {
            let rows = obj[&table].as_array().unwrap();
            assert!(!rows.is_empty(), "table '{table}' has no fixture rows");
        }
    }

    #[test]
    fn export_preserves_nulls_and_types() {
        let fx = Fixture::new();
        let conn = fx.seeded("types.sqlite");
        let envelope = export_value(&conn).unwrap();
        let row = &envelope["collection"][1];

        assert_eq!(row["id"], Value::from(2));
        assert_eq!(row["purchase_price"], Value::Null);
        assert_eq!(row["sale_price"], Value::from(3.0));
        assert_eq!(row["graded"], Value::from(0));
        assert_eq!(row["language"], Value::from("Japanese"));
        assert_eq!(envelope["settings"][1]["value"], Value::from("Pikachu ⚡"));
        assert_eq!(envelope[KEY_FORMAT], Value::from(FORMAT));
        assert_eq!(envelope[KEY_VERSION], Value::from(VERSION));
    }

    #[test]
    fn export_excludes_the_shared_catalog() {
        let fx = Fixture::new();
        let conn = fx.user("catalog.sqlite");
        let envelope = export_value(&conn).unwrap();
        let obj = envelope.as_object().unwrap();

        // The catalog is attached and reachable through temp views, but it is
        // reproducible from upstream — it never belongs in a backup.
        assert!(obj.contains_key("collection"));
        for catalog_table in ["cards", "printings", "sets", "variants"] {
            assert!(
                !obj.contains_key(catalog_table),
                "'{catalog_table}' is catalog data, not collection data"
            );
        }
    }

    #[test]
    fn import_refuses_a_non_empty_target_by_default() {
        let fx = Fixture::new();
        let json = export(&fx.seeded("source.sqlite")).unwrap();
        let mut target = fx.seeded("target.sqlite");

        let err = import(&mut target, &json, OnExisting::Fail).unwrap_err();
        assert!(
            matches!(err, DbError::Conflict(_)),
            "expected a conflict, got {err:?}"
        );
    }

    #[test]
    fn import_replaces_existing_contents() {
        let fx = Fixture::new();
        let source = fx.seeded("source.sqlite");
        let json = export(&source).unwrap();

        // A target holding *different* rows — none of which may survive.
        let mut target = fx.user("target.sqlite");
        target
            .execute(
                "INSERT INTO collection (id, printing_id, acquired_at, source) \
                 VALUES (99, 'sv3pt5-6-normal', '2026-01-01T00:00:00+00:00', 'manual_id')",
                [],
            )
            .unwrap();
        target
            .execute(
                "INSERT INTO settings (key, value) VALUES ('stale', 'yes')",
                [],
            )
            .unwrap();

        import(&mut target, &json, OnExisting::Replace).unwrap();

        let stale: i64 = target
            .query_row("SELECT count(*) FROM collection WHERE id = 99", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stale, 0, "replace must clear pre-existing rows");
        let stale_setting: i64 = target
            .query_row(
                "SELECT count(*) FROM settings WHERE key = 'stale'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale_setting, 0);
        assert_eq!(
            without_timestamp(export_value(&target).unwrap()),
            without_timestamp(export_value(&source).unwrap())
        );
    }

    #[test]
    fn import_is_order_independent_across_foreign_keys() {
        // `collection` references `binders`/`orders`/`batches`; the envelope's
        // keys are alphabetical, so `batches` and `collection` both land
        // before `orders`. Deferred foreign keys are what make that work.
        let fx = Fixture::new();
        let json = export(&fx.seeded("source.sqlite")).unwrap();
        let mut target = fx.user("target.sqlite");

        let summary = import(&mut target, &json, OnExisting::Fail).unwrap();
        assert!(summary.total() > 0);

        let violations: i64 = target
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn import_rejects_a_foreign_envelope() {
        let fx = Fixture::new();
        let mut conn = fx.user("target.sqlite");

        for bad in [
            r#"{"format":"scryfall-dump","version":1}"#,
            r#"{"version":1}"#,
            r#"{"format":"pkdump-collection"}"#,
            r#"{"format":"pkdump-collection","version":99}"#,
            r#"["not","an","object"]"#,
            r#"{"format":"pkdump-collection","version":1,"cards":[]}"#,
            r#"{"format":"pkdump-collection","version":1,"collection":[{"nope":1}]}"#,
            r#"{"format":"pkdump-collection","version":1,"collection":{}}"#,
            "not json at all",
        ] {
            let err = import(&mut conn, bad, OnExisting::Fail).unwrap_err();
            assert!(
                matches!(err, DbError::Import(_)),
                "expected an import error for {bad}, got {err:?}"
            );
        }
    }

    #[test]
    fn a_rejected_import_leaves_the_database_untouched() {
        let fx = Fixture::new();
        let source = fx.seeded("source.sqlite");
        let mut target = fx.seeded("target.sqlite");
        let before = without_timestamp(export_value(&target).unwrap());

        // Valid envelope, one bad column deep inside it.
        let mut envelope = export_value(&source).unwrap();
        envelope["collection"][0]["not_a_column"] = Value::from(1);
        let json = serde_json::to_string(&envelope).unwrap();

        import(&mut target, &json, OnExisting::Replace).unwrap_err();
        assert_eq!(
            without_timestamp(export_value(&target).unwrap()),
            before,
            "a failed import must not delete anything"
        );
    }

    #[test]
    fn an_empty_collection_round_trips() {
        let fx = Fixture::new();
        let source = fx.user("empty.sqlite");
        let json = export(&source).unwrap();

        let mut target = fx.user("target.sqlite");
        let summary = import(&mut target, &json, OnExisting::Fail).unwrap();
        // "Empty" means no collection data. The schema seeds `conditions` on
        // open (pd-s4c2), so a brand-new collection is not a zero-row one.
        let seeded: usize = summary
            .tables
            .iter()
            .filter(|(t, _)| t != "conditions")
            .map(|(_, n)| n)
            .sum();
        assert_eq!(seeded, 0);
        assert_eq!(
            without_timestamp(export_value(&target).unwrap()),
            without_timestamp(export_value(&source).unwrap())
        );
    }

    /// The default refusal asks "would this destroy something", and a fresh
    /// collection is not empty: the schema seeds `conditions` on every open
    /// (pd-s4c2). Tested against a zero-row baseline instead, every single
    /// `pkdump import --json` would refuse a database nobody had put
    /// anything into.
    #[test]
    fn importing_into_a_freshly_created_collection_is_not_a_conflict() {
        let fx = Fixture::new();
        let json = export(&fx.seeded("source.sqlite")).unwrap();

        let mut target = fx.user("target.sqlite");
        assert!(
            crate::conditions::list_all(&target).unwrap().len() == 5,
            "the fixture needs a target that is seeded but otherwise untouched"
        );
        import(&mut target, &json, OnExisting::Fail).unwrap();
    }

    /// ...and the refusal still fires on the seeded table itself once the
    /// collection holds more of it than it was born with. The baseline is
    /// "what a new collection has", not "this table is exempt".
    #[test]
    fn import_refuses_a_target_holding_more_than_its_birth_state() {
        let fx = Fixture::new();
        let json = export(&fx.seeded("source.sqlite")).unwrap();

        let mut target = fx.user("target.sqlite");
        target
            .execute(
                "INSERT INTO conditions (name, multiplier, rank) VALUES ('Graded', 1.5, 9)",
                [],
            )
            .unwrap();

        let err = import(&mut target, &json, OnExisting::Fail).unwrap_err();
        assert!(
            matches!(err, DbError::Conflict(_)),
            "expected a conflict, got {err:?}"
        );
    }

    /// An envelope written before `conditions` lived in the collection
    /// carries no such key, and the import empties every table it does not
    /// mention. Left there, that is not a loud failure — it is every
    /// multiplier silently defaulting to 1.0 on the collection page.
    #[test]
    fn importing_a_pre_move_envelope_leaves_the_multipliers_seeded() {
        let fx = Fixture::new();
        let mut envelope = export_value(&fx.seeded("source.sqlite")).unwrap();
        envelope
            .as_object_mut()
            .unwrap()
            .remove("conditions")
            .expect("the exporter must carry conditions");

        let mut target = fx.user("target.sqlite");
        import(&mut target, &envelope.to_string(), OnExisting::Fail).unwrap();

        let m = crate::conditions::multipliers(&target).unwrap();
        assert_eq!(m.len(), 5);
        assert_eq!(m.get("Damaged"), Some(&0.25));
    }
}
