//! Nessie's version store, made durable (`pkdump_lake.nessie_backup`, ported).
//!
//! Nessie's version store is the only thing that points at the Iceberg metadata
//! files; a backup is a verified `nessie-server-admin export` plus a manifest
//! inventorying every table. Layout, in the backup store:
//!
//! ```text
//! nessie/<backup-id>/export.zip
//! nessie/<backup-id>/manifest.json     written LAST: no manifest, no backup
//! ```
//!
//! The manifest is byte-for-byte what the Python writes (`json.dumps(indent=1,
//! sort_keys=True)`), so either client's backups are read by either's `check`.
//!
//! Exit statuses (`check`, `drill`): 0 good, 3 stale or absent, 4 corrupt, 5 a
//! table could not be read after restore, 1 the question could not be asked.

use std::io::Cursor;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use dumpster_kit_lake::{DirStore, ObjectPurge, ObjectSource, ObjectStore, S3Store};
use iceberg::{Catalog, NamespaceIdent, TableIdent};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const PREFIX: &str = "nessie";
pub const EXPORT: &str = "export.zip";
pub const MANIFEST: &str = "manifest.json";
pub const MANIFEST_FORMAT: i64 = 1;

pub const EXIT_OK: i32 = 0;
pub const EXIT_STALE: i32 = 3;
pub const EXIT_CORRUPT: i32 = 4;
pub const EXIT_UNREADABLE: i32 = 5;

/// ~26h: one nightly backup plus two hours of slack for a long chain.
pub const DEFAULT_MAX_AGE_HOURS: f64 = 26.0;
/// Daily backups kept. Two weeks.
pub const DEFAULT_KEEP: usize = 14;

/// A backup that is not what it claims to be.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BackupError(pub String);

fn err<T>(msg: impl Into<String>) -> Result<T, BackupError> {
    Err(BackupError(msg.into()))
}

impl From<dumpster_kit_lake::LakeError> for BackupError {
    fn from(e: dumpster_kit_lake::LakeError) -> Self {
        BackupError(e.to_string())
    }
}

// --- storage ---------------------------------------------------------------

pub trait Store {
    fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackupError>;
    fn get(&self, key: &str) -> Result<Vec<u8>, BackupError>;
    fn keys(&self, prefix: &str) -> Result<Vec<String>, BackupError>;
    fn delete(&self, key: &str) -> Result<(), BackupError>;
}

/// Any of the kit's stores, which implement writing, reading and purging.
pub struct KitStore<T>(pub T);

impl<T: ObjectStore + ObjectSource + ObjectPurge> Store for KitStore<T> {
    fn put(&self, key: &str, data: Vec<u8>) -> Result<(), BackupError> {
        Ok(ObjectStore::put(&self.0, key, data)?)
    }
    fn get(&self, key: &str) -> Result<Vec<u8>, BackupError> {
        Ok(ObjectSource::get(&self.0, key)?)
    }
    fn keys(&self, prefix: &str) -> Result<Vec<String>, BackupError> {
        Ok(ObjectSource::list_keys(&self.0, prefix)?)
    }
    fn delete(&self, key: &str) -> Result<(), BackupError> {
        Ok(ObjectPurge::delete(&self.0, key)?)
    }
}

/// `s3://bucket/prefix` or `file:///dir`. S3 endpoint and region come from
/// `PKDUMP_NESSIE_BACKUP_S3_ENDPOINT` and `AWS_REGION`/`AWS_DEFAULT_REGION`;
/// credentials from the ambient AWS chain.
pub fn open_store(url: &str) -> Result<Box<dyn Store>, BackupError> {
    if let Some(path) = url.strip_prefix("file://") {
        return Ok(Box::new(KitStore(DirStore::new(path))));
    }
    if let Some(rest) = url.strip_prefix("s3://") {
        let (bucket, base) = rest.split_once('/').unwrap_or((rest, ""));
        if bucket.is_empty() {
            return err(format!("not an s3:// URL: {url:?}"));
        }
        let region = ["AWS_REGION", "AWS_DEFAULT_REGION"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
            .ok_or_else(|| {
                BackupError("AWS_REGION is not set — the backup bucket's region".into())
            })?;
        let endpoint = std::env::var("PKDUMP_NESSIE_BACKUP_S3_ENDPOINT")
            .ok()
            .filter(|v| !v.is_empty());
        let store = S3Store::connect(bucket, &region, base.trim_matches('/'), endpoint.as_deref())?;
        return Ok(Box::new(KitStore(store)));
    }
    err(format!(
        "unsupported store URL {url:?} (want s3://… or file://…)"
    ))
}

// --- the artifact ----------------------------------------------------------

/// The export is a readable, non-empty zip. Returns its entry count.
pub fn verify_export(data: &[u8]) -> Result<usize, BackupError> {
    if data.is_empty() {
        return err("export is empty");
    }
    let mut z = zip::ZipArchive::new(Cursor::new(data))
        .map_err(|_| BackupError("export is not a zip archive".into()))?;
    if z.is_empty() {
        return err("export archive has no entries");
    }
    for i in 0..z.len() {
        let mut f = z
            .by_index(i)
            .map_err(|e| BackupError(format!("export entry {i} is unreadable: {e}")))?;
        let name = f.name().to_string();
        std::io::copy(&mut f, &mut std::io::sink())
            .map_err(|_| BackupError(format!("export entry {name:?} fails its CRC")))?;
    }
    Ok(z.len())
}

/// One table in the inventory. Fields are alphabetical: that is the order
/// `sort_keys=True` writes them in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableEntry {
    pub identifier: String,
    pub metadata_location: String,
    pub rows: i64,
    pub snapshot_id: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRecord {
    pub bytes: u64,
    pub key: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub export: ExportRecord,
    pub format: i64,
    pub id: String,
    pub nessie_version: String,
    pub tables: Vec<TableEntry>,
}

/// `json.dumps(value, indent=1, sort_keys=True)`.
pub fn to_python_json<T: Serialize>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    value.serialize(&mut ser).expect("serialize to a Vec");
    out
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Live data rows of a snapshot, summed from its manifests: metadata only, no
/// data file is opened. A delete manifest would make the sum wrong, so one is
/// an error rather than a guess.
async fn manifest_row_count(
    table: &iceberg::table::Table,
    snap: &iceberg::spec::SnapshotRef,
    name: &str,
) -> Result<i64, BackupError> {
    let ic = |e: iceberg::Error| BackupError(format!("{name}: iceberg: {e}"));
    let list = table.manifest_list_reader(snap).load().await.map_err(ic)?;
    let mut rows: u64 = 0;
    for mf in list.entries() {
        if mf.content != iceberg::spec::ManifestContentType::Data {
            return err(format!(
                "{name}: snapshot {} carries no total-records and has delete manifests",
                snap.snapshot_id()
            ));
        }
        let manifest = mf.load_manifest(table.file_io()).await.map_err(ic)?;
        rows += manifest
            .entries()
            .iter()
            .filter(|e| e.is_alive())
            .map(|e| e.record_count())
            .sum::<u64>();
    }
    i64::try_from(rows).map_err(|_| BackupError(format!("{name}: row count {rows} overflows")))
}

/// Every table the live catalog holds: where its metadata is, and how big. Row
/// counts come from the snapshot summary — metadata only.
pub async fn inventory(cat: &dyn Catalog) -> Result<Vec<TableEntry>, BackupError> {
    let ic = |e: iceberg::Error| BackupError(format!("iceberg: {e}"));
    let mut out = Vec::new();
    for ns in cat.list_namespaces(None).await.map_err(ic)? {
        for ident in cat.list_tables(&ns).await.map_err(ic)? {
            let table = cat.load_table(&ident).await.map_err(ic)?;
            let name = ident_string(&ident);
            let (mut rows, mut snap_id) = (0, None);
            if let Some(snap) = table.metadata().current_snapshot() {
                snap_id = Some(snap.snapshot_id());
                let total = snap.summary().additional_properties.get("total-records");
                rows = match total {
                    Some(t) => t.parse::<i64>().map_err(|_| {
                        BackupError(format!("{name}: total-records {t:?} is not a number"))
                    })?,
                    // Writers that omit the summary count (iceberg-rust) leave
                    // the manifests as the only record of it.
                    None => manifest_row_count(&table, snap, &name).await?,
                };
            }
            out.push(TableEntry {
                identifier: name,
                metadata_location: table
                    .metadata_location()
                    .ok_or_else(|| {
                        BackupError(format!(
                            "{}: catalog returned no metadata location",
                            ident.name()
                        ))
                    })?
                    .to_string(),
                snapshot_id: snap_id,
                rows,
            });
        }
    }
    out.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    Ok(out)
}

fn ident_string(ident: &TableIdent) -> String {
    let mut parts: Vec<String> = ident.namespace().as_ref().to_vec();
    parts.push(ident.name().to_string());
    parts.join(".")
}

pub fn format_id(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

fn id_is_valid(id: &str) -> bool {
    let b = id.as_bytes();
    b.len() == 16
        && b[8] == b'T'
        && b[15] == b'Z'
        && b[..8].iter().chain(&b[9..15]).all(u8::is_ascii_digit)
}

pub fn parse_id(id: &str) -> Result<DateTime<Utc>, BackupError> {
    if !id_is_valid(id) {
        return err(format!("bad backup id {id:?}"));
    }
    NaiveDateTime::parse_from_str(id, "%Y%m%dT%H%M%SZ")
        .map(|n| Utc.from_utc_datetime(&n))
        .map_err(|_| BackupError(format!("bad backup id {id:?}")))
}

/// Verify, upload, and only then publish the manifest; then prune.
pub fn put_backup(
    export: Vec<u8>,
    store: &dyn Store,
    tables: Vec<TableEntry>,
    backup_id: &str,
    nessie_version: &str,
    keep: usize,
) -> Result<Manifest, BackupError> {
    if keep < 1 {
        return err("--keep must be at least 1");
    }
    parse_id(backup_id)?;
    verify_export(&export)?;
    let base = format!("{PREFIX}/{backup_id}");
    let manifest = Manifest {
        export: ExportRecord {
            bytes: export.len() as u64,
            key: format!("{base}/{EXPORT}"),
            sha256: sha256_hex(&export),
        },
        format: MANIFEST_FORMAT,
        id: backup_id.to_string(),
        nessie_version: nessie_version.to_string(),
        tables,
    };
    store.put(&format!("{base}/{EXPORT}"), export)?;
    // Read it back before blessing it: a PUT that returned is not an object that
    // is there.
    if sha256_hex(&store.get(&format!("{base}/{EXPORT}"))?) != manifest.export.sha256 {
        return err("uploaded export does not read back identically");
    }
    store.put(&format!("{base}/{MANIFEST}"), to_python_json(&manifest))?;
    prune(store, keep)?;
    Ok(manifest)
}

fn key_parts(k: &str) -> Vec<&str> {
    k.split('/').collect()
}

/// Complete backups (those with a manifest), oldest first.
pub fn backup_ids(store: &dyn Store) -> Result<Vec<String>, BackupError> {
    let mut ids = std::collections::BTreeSet::new();
    for k in store.keys(PREFIX)? {
        let p = key_parts(&k);
        if p.len() == 3 && p[2] == MANIFEST && id_is_valid(p[1]) {
            ids.insert(p[1].to_string());
        }
    }
    Ok(ids.into_iter().collect())
}

/// Delete all but the newest `keep` complete backups, and any incomplete one
/// older than the newest complete backup. Returns the ids removed.
pub fn prune(store: &dyn Store, keep: usize) -> Result<Vec<String>, BackupError> {
    let ids = backup_ids(store)?;
    let mut doomed: std::collections::BTreeSet<String> = if ids.len() > keep {
        ids[..ids.len() - keep].iter().cloned().collect()
    } else {
        Default::default()
    };
    let newest = ids.last().cloned();
    for k in store.keys(PREFIX)? {
        let p = key_parts(&k);
        if p.len() != 3 || !id_is_valid(p[1]) {
            continue; // never touch what we do not recognise
        }
        let incomplete =
            !ids.iter().any(|i| i == p[1]) && newest.as_deref().is_some_and(|n| p[1] < n);
        if doomed.contains(p[1]) || incomplete {
            store.delete(&k)?;
            doomed.insert(p[1].to_string());
        }
    }
    Ok(doomed.into_iter().collect())
}

pub fn load_manifest(store: &dyn Store, backup_id: &str) -> Result<Manifest, BackupError> {
    let raw = store.get(&format!("{PREFIX}/{backup_id}/{MANIFEST}"))?;
    let m: Manifest = serde_json::from_slice(&raw)
        .map_err(|e| BackupError(format!("manifest of {backup_id} is unreadable: {e}")))?;
    if m.format != MANIFEST_FORMAT || m.id != backup_id {
        return err(format!(
            "manifest of {backup_id} is not format {MANIFEST_FORMAT} for that id"
        ));
    }
    Ok(m)
}

/// Restore-grade verification short of running Nessie: fetch the export and
/// hold it to its manifest.
pub fn verify_backup(
    store: &dyn Store,
    backup_id: &str,
) -> Result<(Manifest, Vec<u8>), BackupError> {
    let m = load_manifest(store, backup_id)?;
    let data = store.get(&m.export.key)?;
    if data.len() as u64 != m.export.bytes {
        return err(format!(
            "{backup_id}: export is {} bytes, manifest says {}",
            data.len(),
            m.export.bytes
        ));
    }
    if sha256_hex(&data) != m.export.sha256 {
        return err(format!(
            "{backup_id}: export sha256 does not match its manifest"
        ));
    }
    verify_export(&data)?;
    if m.tables.is_empty() {
        return err(format!("{backup_id}: manifest inventories no tables"));
    }
    Ok((m, data))
}

/// The alarm. Returns (exit status, one line saying why).
pub fn check(
    store: &dyn Store,
    now: DateTime<Utc>,
    max_age_hours: f64,
) -> Result<(i32, String), BackupError> {
    let ids = backup_ids(store)?;
    let Some(newest) = ids.last() else {
        return Ok((EXIT_STALE, "no Nessie backup exists".into()));
    };
    if let Err(e) = verify_backup(store, newest) {
        return Ok((
            EXIT_CORRUPT,
            format!("newest backup {newest} cannot be verified: {e}"),
        ));
    }
    let hours = (now - parse_id(newest)?).num_milliseconds() as f64 / 3_600_000.0;
    if hours > max_age_hours {
        return Ok((
            EXIT_STALE,
            format!("newest backup {newest} is {hours:.1}h old (limit {max_age_hours}h)"),
        ));
    }
    Ok((
        EXIT_OK,
        format!("newest backup {newest} is {hours:.1}h old and verifies"),
    ))
}

// --- the drill -------------------------------------------------------------

/// Hold a restored catalog to the manifest. Returns the failures (empty = every
/// table read back).
pub async fn drill(
    cat: &dyn Catalog,
    manifest: &Manifest,
    require_namespaces: &[String],
) -> Vec<String> {
    let mut failures = Vec::new();
    if manifest.tables.is_empty() {
        return vec!["manifest inventories no tables: a drill over nothing proves nothing".into()];
    }
    let present: std::collections::BTreeSet<&str> = manifest
        .tables
        .iter()
        .map(|t| {
            t.identifier
                .rsplit_once('.')
                .map_or(t.identifier.as_str(), |(ns, _)| ns)
        })
        .collect();
    for ns in require_namespaces {
        if !present.contains(ns.as_str()) {
            failures.push(format!("namespace {ns:?} has no table in the backup"));
        }
    }
    for t in &manifest.tables {
        if let Err(e) = drill_one(cat, t, &mut failures).await {
            failures.push(format!("{}: cannot be read: {e}", t.identifier));
        }
    }
    failures
}

async fn drill_one(
    cat: &dyn Catalog,
    t: &TableEntry,
    failures: &mut Vec<String>,
) -> Result<(), String> {
    let mut parts: Vec<String> = t.identifier.split('.').map(str::to_string).collect();
    let name = parts.pop().unwrap_or_default();
    let ident = TableIdent::new(
        NamespaceIdent::from_vec(parts).map_err(|e| e.to_string())?,
        name,
    );
    let table = cat.load_table(&ident).await.map_err(|e| e.to_string())?;
    let loc = table.metadata_location().unwrap_or_default();
    if loc != t.metadata_location {
        failures.push(format!(
            "{}: restored catalog points at {loc}, backup recorded {}",
            t.identifier, t.metadata_location
        ));
        return Ok(());
    }
    let rows = if table.metadata().current_snapshot().is_none() {
        0
    } else {
        use futures::TryStreamExt;
        let first = table
            .metadata()
            .current_schema()
            .as_struct()
            .fields()
            .first()
            .map(|f| f.name.clone())
            .ok_or("table has no columns")?;
        let scan = table
            .scan()
            .select([first])
            .build()
            .map_err(|e| e.to_string())?;
        let batches: Vec<_> = scan
            .to_arrow()
            .await
            .map_err(|e| e.to_string())?
            .try_collect()
            .await
            .map_err(|e| e.to_string())?;
        batches.iter().map(|b| b.num_rows() as i64).sum()
    };
    if rows != t.rows {
        failures.push(format!(
            "{}: read {rows} rows, backup recorded {}",
            t.identifier, t.rows
        ));
    }
    Ok(())
}

// --- the fallback ----------------------------------------------------------

/// `<ns>/<table>_<uuid32>/metadata/NNNNN-<uuid>.metadata.json` → (ns, table, version).
fn parse_metadata_key(key: &str) -> Option<(String, String, u64)> {
    let p: Vec<&str> = key.split('/').collect();
    let n = p.len();
    if n < 4 || p[n - 2] != "metadata" {
        return None;
    }
    let stem = p[n - 1].strip_suffix(".metadata.json")?;
    let (ver, uuid) = stem.split_once('-')?;
    if ver.is_empty()
        || !ver.bytes().all(|b| b.is_ascii_digit())
        || uuid.is_empty()
        || !uuid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
    {
        return None;
    }
    let dir = p[n - 3];
    let table = dir
        .len()
        .checked_sub(33)
        .filter(|&i| i > 0 && dir.as_bytes()[i] == b'_')?;
    let hex = &dir[table + 1..];
    if !hex
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    Some((
        p[n - 4].to_string(),
        dir[..table].to_string(),
        ver.parse().ok()?,
    ))
}

/// (namespace, table) → key of its newest metadata.json, from a listing of the
/// warehouse.
pub fn discover_tables(keys: &[String]) -> std::collections::BTreeMap<(String, String), String> {
    let mut best: std::collections::BTreeMap<(String, String), (u64, String)> = Default::default();
    for k in keys {
        if let Some((ns, tbl, v)) = parse_metadata_key(k) {
            let slot = best.entry((ns, tbl)).or_insert((v, k.clone()));
            if v > slot.0 {
                *slot = (v, k.clone());
            }
        }
    }
    best.into_iter().map(|(i, (_, k))| (i, k)).collect()
}

pub async fn reregister(
    cat: &dyn Catalog,
    found: &std::collections::BTreeMap<(String, String), String>,
    location_root: &str,
) -> Result<Vec<String>, BackupError> {
    let mut done = Vec::new();
    for ((ns, name), key) in found {
        let nsi = NamespaceIdent::new(ns.clone());
        // Already exists is the usual case.
        let _ = cat.create_namespace(&nsi, Default::default()).await;
        let ident = TableIdent::new(nsi, name.clone());
        cat.register_table(
            &ident,
            format!("{}/{key}", location_root.trim_end_matches('/')),
        )
        .await
        .map_err(|e| BackupError(format!("{ns}.{name}: {e}")))?;
        done.push(format!("{ns}.{name}"));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    pub(crate) fn zip_bytes() -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            z.start_file(
                "a.txt",
                zip::write::FileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
            z.write_all(b"nessie").unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    fn store() -> (tempfile::TempDir, KitStore<DirStore>) {
        let d = tempfile::tempdir().unwrap();
        let s = KitStore(DirStore::new(d.path()));
        (d, s)
    }

    fn tables() -> Vec<TableEntry> {
        vec![TableEntry {
            identifier: "catalog.prices".into(),
            metadata_location: "s3://w/catalog/prices_0123456789abcdef0123456789abcdef/metadata/00001-aa.metadata.json".into(),
            rows: 3,
            snapshot_id: Some(7),
        }]
    }

    #[test]
    fn ids_are_utc_timestamps() {
        assert!(parse_id("20261011T071500Z").is_ok());
        assert!(parse_id("2026-10-11").is_err());
        assert!(parse_id("20261011T0715000").is_err());
    }

    #[test]
    fn a_non_zip_and_an_empty_export_are_refused() {
        assert!(verify_export(b"").is_err());
        assert!(verify_export(b"not a zip").is_err());
        assert_eq!(verify_export(&zip_bytes()).unwrap(), 1);
    }

    #[test]
    fn a_corrupted_entry_fails_its_crc() {
        let mut z = zip_bytes();
        let at = z.windows(6).position(|w| w == b"nessie").unwrap();
        z[at] ^= 0xff;
        assert!(verify_export(&z).unwrap_err().0.contains("CRC"));
    }

    #[test]
    fn put_then_check_round_trips_and_prunes_to_keep() {
        let (_d, s) = store();
        for day in 1..=4 {
            put_backup(
                zip_bytes(),
                &s,
                tables(),
                &format!("2026101{day}T071500Z"),
                "0.104.3",
                2,
            )
            .unwrap();
        }
        assert_eq!(
            backup_ids(&s).unwrap(),
            ["20261013T071500Z", "20261014T071500Z"]
        );
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 9, 15, 0).unwrap();
        let (code, why) = check(&s, now, 26.0).unwrap();
        assert_eq!(
            (code, why.as_str()),
            (0, "newest backup 20261014T071500Z is 2.0h old and verifies")
        );
        let late = Utc.with_ymd_and_hms(2026, 10, 16, 9, 15, 0).unwrap();
        assert_eq!(check(&s, late, 26.0).unwrap().0, EXIT_STALE);
    }

    #[test]
    fn no_backup_is_stale_and_a_flipped_byte_is_corrupt() {
        let (d, s) = store();
        let now = Utc.with_ymd_and_hms(2026, 10, 14, 9, 15, 0).unwrap();
        assert_eq!(
            check(&s, now, 26.0).unwrap(),
            (EXIT_STALE, "no Nessie backup exists".into())
        );
        put_backup(zip_bytes(), &s, tables(), "20261014T071500Z", "0.104.3", 14).unwrap();
        let p = d.path().join("nessie/20261014T071500Z/export.zip");
        let mut b = std::fs::read(&p).unwrap();
        b[0] ^= 1;
        std::fs::write(&p, b).unwrap();
        assert_eq!(check(&s, now, 26.0).unwrap().0, EXIT_CORRUPT);
    }

    #[test]
    fn an_incomplete_backup_is_ignored_and_pruned_once_a_newer_one_completes() {
        let (d, s) = store();
        s.put("nessie/20261010T071500Z/export.zip", b"half".to_vec())
            .unwrap();
        assert!(backup_ids(&s).unwrap().is_empty());
        put_backup(zip_bytes(), &s, tables(), "20261011T071500Z", "x", 14).unwrap();
        assert!(
            !d.path().join("nessie/20261010T071500Z").exists()
                || s.keys("nessie/20261010").unwrap().is_empty()
        );
        s.put("nessie/notes.txt", b"keep".to_vec()).unwrap();
        prune(&s, 1).unwrap();
        assert!(d.path().join("nessie/notes.txt").exists());
    }

    #[test]
    fn a_manifest_with_no_tables_does_not_verify() {
        let (_d, s) = store();
        put_backup(zip_bytes(), &s, vec![], "20261014T071500Z", "x", 14).unwrap();
        assert!(
            verify_backup(&s, "20261014T071500Z")
                .unwrap_err()
                .0
                .contains("no tables")
        );
    }

    #[test]
    fn metadata_keys_are_discovered_newest_version_per_table() {
        let u = "0123456789abcdef0123456789abcdef";
        let keys: Vec<String> = [
            format!("lake/catalog/prices_{u}/metadata/00001-aaaa-bbbb.metadata.json"),
            format!("lake/catalog/prices_{u}/metadata/00010-cccc-dddd.metadata.json"),
            format!("lake/catalog/prices_{u}/metadata/snap-1.avro"),
            format!("catalog/sealed_prices_{u}/metadata/00002-ab.metadata.json"),
            "stray/metadata/00001-aa.metadata.json".to_string(),
        ]
        .into();
        let found = discover_tables(&keys);
        assert_eq!(found.len(), 2);
        assert!(found[&("catalog".into(), "prices".into())].contains("00010-"));
        assert!(found.contains_key(&("catalog".into(), "sealed_prices".into())));
    }
}
