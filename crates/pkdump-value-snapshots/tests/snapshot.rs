//! The SQLite half, over hand-built databases carrying only the columns the
//! transform's SQL touches. The row-identity claim against Rust
//! `snapshot_today` AND against the Python job is `tests/lake/value_snapshots.sh`
//! (§5, §5d): this file holds the refusals and the arithmetic's edges.

use std::collections::HashMap;
use std::path::Path;

use pkdump_value_snapshots::lake::{CardPrices, SealedPrices, nessie_api_base};
use pkdump_value_snapshots::snapshot::{Tenant, snapshot, tenants};
use pkdump_value_snapshots::{EXIT_FAILED, EXIT_OK, EXIT_PARTIAL, Outcome, report};
use rusqlite::Connection;

fn shared(path: &Path) {
    Connection::open(path)
        .unwrap()
        .execute_batch(
            "CREATE TABLE cards (card_id TEXT PRIMARY KEY, set_code TEXT);
             CREATE TABLE printings (printing_id TEXT PRIMARY KEY, card_id TEXT,
                                     tcgplayer_product_id INTEGER, sub_type_name TEXT);
             CREATE TABLE catalog_price_overrides (printing_id TEXT PRIMARY KEY, price REAL);
             INSERT INTO cards VALUES ('c1','sv1'), ('c2','sv2');
             INSERT INTO printings VALUES ('p1','c1',100,'Normal'), ('p2','c2',200,'Holofoil');",
        )
        .unwrap();
}

fn tenant_db(path: &Path, sealed_rows: i64, shipped_thru: i64) {
    Connection::open(path)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TABLE collection_value_snapshot (date TEXT, dimension TEXT, bucket TEXT,
                market_value REAL, cost_basis REAL, card_count INTEGER);
             CREATE TABLE collection_value_snapshot_run (date TEXT PRIMARY KEY, artefact TEXT,
                lake_ref TEXT, rows INTEGER, written_at TEXT);
             CREATE TABLE conditions (name TEXT PRIMARY KEY, multiplier REAL);
             CREATE TABLE manual_prices (printing_id TEXT, price REAL, observed_at TEXT);
             CREATE TABLE user_printings (printing_id TEXT, card_id TEXT);
             CREATE TABLE zone_holdings (id INTEGER, printing_id TEXT, status TEXT,
                purchase_price REAL, binder_id INTEGER, condition TEXT);
             CREATE TABLE zone_sealed_holdings (id INTEGER, product_id INTEGER, status TEXT,
                quantity INTEGER, purchase_price REAL);
             CREATE TABLE zone_holdings_run (dataset TEXT, max_seq INTEGER, read_at TEXT,
                parts INTEGER, per_source TEXT);
             CREATE TABLE ownership_outbox_cursor (id INTEGER, shipped_thru INTEGER);
             INSERT INTO conditions VALUES ('NM', 1.0), ('LP', 0.5);
             INSERT INTO zone_holdings VALUES
                (1,'p1','owned',1.0,7,'NM'), (2,'p1','owned',2.0,NULL,'LP'),
                (3,'p2','owned',3.0,NULL,'NM'), (4,'p2','sold',9.0,NULL,'NM');
             INSERT INTO zone_sealed_holdings VALUES
                (1,900,'owned',3,10.0), (2,901,'owned',2,5.0);
             INSERT INTO zone_holdings_run VALUES
                ('holdings',50,'2026-01-01T00:00:00Z',2,
                 '{{\"collection\":4,\"sealed_collection\":{sealed_rows}}}');
             INSERT INTO ownership_outbox_cursor VALUES (1, {shipped_thru});"
        ))
        .unwrap();
}

fn tenant(dir: &Path) -> Tenant {
    Tenant {
        handle: "alice".into(),
        database_id: "01ALICE".into(),
        path: dir.join("alice.sqlite"),
    }
}

fn prices() -> (CardPrices, SealedPrices) {
    let cards: CardPrices = HashMap::from([((100, "Normal".to_string()), 10.0)]);
    // 901 is unquoted: skipped by the sum, counted in the units.
    let sealed: SealedPrices = HashMap::from([(900, 40.0)]);
    (cards, sealed)
}

fn rows(path: &Path) -> Vec<(String, Option<String>, f64, f64, i64)> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT dimension, bucket, market_value, cost_basis, card_count
               FROM collection_value_snapshot ORDER BY dimension, bucket",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

#[test]
fn values_cards_and_sealed_by_the_python_rules() {
    let dir = tempfile::tempdir().unwrap();
    shared(&dir.path().join("shared.sqlite"));
    tenant_db(&dir.path().join("alice.sqlite"), 2, 50);
    let (cards, sealed) = prices();
    let t = tenant(dir.path());

    let written = snapshot(
        &t,
        &dir.path().join("shared.sqlite"),
        &cards,
        &sealed,
        "2026-01-02",
        "catalog.prices",
        "main@abc",
        false,
    )
    .unwrap();
    // all + 2 sets + 1 binder + sealed.
    assert_eq!(written, 5);

    let got = rows(&t.path);
    let find = |dim: &str, bucket: Option<&str>| {
        got.iter()
            .find(|r| r.0 == dim && r.1.as_deref() == bucket)
            .unwrap_or_else(|| panic!("no {dim} {bucket:?} in {got:?}"))
            .clone()
    };
    // p1 x2 at 10.0 with multipliers 1.0 and 0.5; p2 unpriced (no lake price,
    // no override) so it adds nothing to the value but is counted; the sold
    // copy is not owned.
    let all = find("all", None);
    assert_eq!((all.2, all.3, all.4), (15.0, 6.0, 3));
    let set2 = find("set", Some("sv2"));
    assert_eq!(
        (set2.2, set2.4),
        (0.0, 1),
        "unpriced is zero in the sum, counted"
    );
    assert_eq!(find("binder", Some("7")).2, 10.0);
    // 3 units at 40, the unquoted lot still counted: 5 units, cost 3*10 + 2*5.
    let sealed_row = find("sealed", None);
    assert_eq!((sealed_row.2, sealed_row.3, sealed_row.4), (120.0, 40.0, 5));

    let (artefact, lake_ref, n): (String, String, i64) = Connection::open(&t.path)
        .unwrap()
        .query_row(
            "SELECT artefact, lake_ref, rows FROM collection_value_snapshot_run",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (artefact.as_str(), lake_ref.as_str(), n),
        ("catalog.prices", "main@abc", 5)
    );

    // Idempotent: a re-run replaces the date rather than doubling it.
    snapshot(
        &t,
        &dir.path().join("shared.sqlite"),
        &cards,
        &sealed,
        "2026-01-02",
        "catalog.prices",
        "main@abc",
        false,
    )
    .unwrap();
    assert_eq!(rows(&t.path).len(), 5);
}

#[test]
fn a_dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    shared(&dir.path().join("shared.sqlite"));
    tenant_db(&dir.path().join("alice.sqlite"), 2, 50);
    let (cards, sealed) = prices();
    let t = tenant(dir.path());
    let n = snapshot(
        &t,
        &dir.path().join("shared.sqlite"),
        &cards,
        &sealed,
        "2026-01-02",
        "catalog.prices",
        "main@abc",
        true,
    )
    .unwrap();
    assert_eq!(n, 5);
    assert!(rows(&t.path).is_empty());
}

#[test]
fn a_stale_or_mismatched_zone_read_is_refused_not_valued() {
    let dir = tempfile::tempdir().unwrap();
    shared(&dir.path().join("shared.sqlite"));
    let (cards, sealed) = prices();
    let t = tenant(dir.path());
    let run = |t: &Tenant| {
        snapshot(
            t,
            &dir.path().join("shared.sqlite"),
            &cards,
            &sealed,
            "2026-01-02",
            "catalog.prices",
            "main@abc",
            false,
        )
    };

    // The shipper has put 60 into the zone; the read stopped at 50.
    tenant_db(&t.path, 2, 60);
    let e = run(&t).unwrap_err().to_string();
    assert!(e.contains("predates the last ship"), "{e}");
    assert!(rows(&t.path).is_empty());

    // Sealed lots staged by a different read than the cards.
    std::fs::remove_file(&t.path).unwrap();
    tenant_db(&t.path, 9, 50);
    let e = run(&t).unwrap_err().to_string();
    assert!(e.contains("not materialised together"), "{e}");

    // A run row from a build that counted differently.
    Connection::open(&t.path)
        .unwrap()
        .execute("UPDATE zone_holdings_run SET per_source = '{}'", [])
        .unwrap();
    let e = run(&t).unwrap_err().to_string();
    assert!(e.contains("no source"), "{e}");

    // Never read back at all.
    Connection::open(&t.path)
        .unwrap()
        .execute("DELETE FROM zone_holdings_run", [])
        .unwrap();
    let e = run(&t).unwrap_err().to_string();
    assert!(e.contains("no staged zone holdings"), "{e}");

    // No database.
    let ghost = Tenant {
        path: dir.path().join("ghost.sqlite"),
        ..tenant(dir.path())
    };
    assert!(
        run(&ghost)
            .unwrap_err()
            .to_string()
            .contains("no database at")
    );
}

#[test]
fn only_active_users_are_walked_in_registry_order() {
    let dir = tempfile::tempdir().unwrap();
    Connection::open(dir.path().join("registry.sqlite"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE user (handle TEXT, database_id TEXT, state TEXT, created_at TEXT);
             INSERT INTO user VALUES ('bob','B','active','2026-02-01'),
                                     ('alice','A','active','2026-01-01'),
                                     ('gone','G','detached','2026-01-01');",
        )
        .unwrap();
    let all = tenants(dir.path(), None).unwrap();
    assert_eq!(
        all.iter().map(|t| t.handle.as_str()).collect::<Vec<_>>(),
        ["alice", "bob"]
    );
    assert!(all[0].path.ends_with("tenants/A.sqlite"));

    let only = tenants(dir.path(), Some(&["bob".to_string()])).unwrap();
    assert_eq!(only.len(), 1);
    let e = tenants(dir.path(), Some(&["gone".to_string()]))
        .unwrap_err()
        .to_string();
    assert!(e.contains("no active user"), "{e}");
    let e = tenants(&dir.path().join("nowhere"), None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("no registry"), "{e}");
}

#[test]
fn exit_statuses_follow_the_pythons_three_answers() {
    let dir = tempfile::tempdir().unwrap();
    let outcome = |error: Option<&str>| Outcome {
        tenant: tenant(dir.path()),
        error: error.map(str::to_string),
    };
    assert_eq!(report(&[outcome(None), outcome(None)], "d"), EXIT_OK);
    assert_eq!(
        report(&[outcome(None), outcome(Some("x"))], "d"),
        EXIT_PARTIAL
    );
    // "Some tenants" and "nobody" are different nights.
    assert_eq!(report(&[outcome(Some("x"))], "d"), EXIT_FAILED);
}

#[test]
fn nessie_is_found_beside_the_iceberg_endpoint() {
    assert_eq!(
        nessie_api_base("http://nessie:19120/iceberg/").unwrap(),
        "http://nessie:19120/api/v2"
    );
    assert!(nessie_api_base("http://nessie:19120/").is_err());
}
