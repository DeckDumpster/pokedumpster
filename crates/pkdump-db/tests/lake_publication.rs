//! `lake_publication` is declared in two places, and they must not drift.
//!
//! The transform tier (pd-ruwh) is a Python job — `lake/src/pkdump_lake/
//! value_snapshot.py` — because reading Iceberg means PyIceberg, and the
//! standing decision is no JVM and no second Iceberg implementation in our own
//! jobs. It writes tenant databases, so it has to be able to create the
//! provenance table itself: a transform that refuses to run until the Rust
//! binary has opened every tenant file since the table was added is a worse
//! failure than an idempotent `CREATE`.
//!
//! That leaves the same DDL written twice, in two languages, which is the
//! shape that goes quiet. It is held together the way `HANDLE_RULE` and the
//! registry `CHECK` are held together — not by sharing code, which they
//! cannot, but by a test that reads both sides. Change one spelling and this
//! fails naming the other.

use std::path::{Path, PathBuf};

const SCHEMA_USER: &str = include_str!("../src/schema_user.sql");

/// The repository root, from this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root is two levels above pkdump-db")
}

/// The `CREATE TABLE ... lake_publication (...)` body, reduced to the tokens
/// that define the table. Comments, indentation and the trailing semicolon are
/// presentation; the columns and the key are the contract.
fn normalize(source: &str) -> String {
    const HEAD: &str = "CREATE TABLE IF NOT EXISTS lake_publication";
    let start = source
        .find(HEAD)
        .unwrap_or_else(|| panic!("no {HEAD} in the source read"));
    // Depth-counted rather than "up to the first `)`": the statement ends with
    // `PRIMARY KEY (artefact, date))`, so the first close paren is not the end.
    let mut depth = 0usize;
    let mut end = None;
    for (offset, ch) in source[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + offset + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let body = &source[start..end.expect("unterminated lake_publication DDL")];
    body.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn lake_publication_ddl_matches_schema_user() {
    let job = repo_root().join("lake/src/pkdump_lake/value_snapshot.py");
    let python =
        std::fs::read_to_string(&job).unwrap_or_else(|e| panic!("reading {}: {e}", job.display()));

    assert_eq!(
        normalize(SCHEMA_USER),
        normalize(&python),
        "the lake_publication DDL in schema_user.sql and in {} have drifted. \
         They are the same table written twice because the Python transform \
         must be able to create it; change both or neither.",
        job.display()
    );
}

#[test]
fn the_schema_actually_creates_it() {
    // The pair test above compares two strings, which would still pass if both
    // were nonsense. This one applies the real schema and asks SQLite.
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared.sqlite");
    pkdump_db::open_shared(&shared).unwrap();
    let conn = pkdump_db::connect_user(&dir.path().join("user.sqlite"), &shared).unwrap();

    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('lake_publication') ORDER BY cid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();

    assert_eq!(
        columns,
        vec!["artefact", "date", "lake_ref", "published_at"],
        "lake_publication is not the shape the transform writes"
    );
}
