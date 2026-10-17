//! Parity with the Python lake jobs, against output the Python recorded.
//!
//! `tests/fixtures/` was produced by running `pkdump_lake.freshness.judge` and
//! `pkdump_lake.nessie_backup.{put_backup,check}` on fixed inputs (the recorder
//! stubs only the catalog client, which these pure paths never touch). The Rust
//! must reproduce the text, the exit statuses and the manifest bytes exactly,
//! and must read a backup the Python wrote — both clients share one bucket until
//! the timers switch.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, Utc};
use dumpster_kit_lake::DirStore;
use pkdump_lakejobs::freshness::{self, Newest};
use pkdump_lakejobs::nessie_backup::{self as nb, KitStore, Store, TableEntry};
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn json(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(fixtures().join(name)).unwrap()).unwrap()
}

#[test]
fn freshness_prints_what_the_python_printed() {
    let cases = json("freshness_cases.json");
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), 8, "the recording changed shape");
    let as_of: NaiveDate = "2026-08-13".parse().unwrap();
    for c in cases {
        let newest = match c["newest"].as_str().unwrap() {
            "missing" => Newest::NoTable,
            "none" => Newest::NoPartition,
            d => Newest::Day(d.parse().unwrap()),
        };
        let v = freshness::judge("catalog.prices", "main", as_of, 2, &newest);
        let join = |ls: &[String]| ls.iter().map(|l| format!("{l}\n")).collect::<String>();
        assert_eq!(
            join(&v.stdout),
            c["stdout"].as_str().unwrap(),
            "stdout for {newest:?}"
        );
        assert_eq!(
            join(&v.stderr),
            c["stderr"].as_str().unwrap(),
            "stderr for {newest:?}"
        );
        assert_eq!(
            i64::from(v.code),
            c["code"].as_i64().unwrap(),
            "exit for {newest:?}"
        );
    }
}

fn python_store() -> (tempfile::TempDir, KitStore<DirStore>) {
    let d = tempfile::tempdir().unwrap();
    copy_dir(&fixtures().join("python_backup"), d.path());
    let s = KitStore(DirStore::new(d.path()));
    (d, s)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

#[test]
fn a_backup_the_python_wrote_verifies_and_is_judged_the_same() {
    let (_d, s) = python_store();
    assert_eq!(nb::backup_ids(&s).unwrap(), ["20261011T071500Z"]);
    let (m, _) = nb::verify_backup(&s, "20261011T071500Z").unwrap();
    assert_eq!(m.tables.len(), 2);
    let checks = json("check_cases.json");
    for (label, c) in checks.as_object().unwrap() {
        let now: DateTime<Utc> = DateTime::parse_from_rfc3339(c["now"].as_str().unwrap())
            .unwrap()
            .into();
        let (code, why) = nb::check(&s, now, 26.0).unwrap();
        assert_eq!(i64::from(code), c["code"].as_i64().unwrap(), "{label}");
        assert_eq!(why, c["why"].as_str().unwrap(), "{label}");
    }
}

#[test]
fn the_manifest_is_byte_identical_to_the_pythons() {
    let d = tempfile::tempdir().unwrap();
    let s = KitStore(DirStore::new(d.path()));
    let export = std::fs::read(fixtures().join("export.zip")).unwrap();
    let tables: Vec<TableEntry> =
        serde_json::from_slice(&std::fs::read(fixtures().join("inventory.json")).unwrap()).unwrap();
    nb::put_backup(
        export.clone(),
        &s,
        tables,
        "20261011T071500Z",
        "0.104.3",
        14,
    )
    .unwrap();

    let key = "nessie/20261011T071500Z";
    let want = std::fs::read(
        fixtures()
            .join("python_backup")
            .join(key)
            .join("manifest.json"),
    )
    .unwrap();
    assert_eq!(s.get(&format!("{key}/manifest.json")).unwrap(), want);
    assert_eq!(s.get(&format!("{key}/export.zip")).unwrap(), export);
}
