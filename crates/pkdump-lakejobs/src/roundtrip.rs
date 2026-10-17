//! The round-trip proof: write, read, commit again, and time-travel back
//! (`pkdump_lake.roundtrip`, ported).
//!
//! Time travel is the primitive Nessie is bought for, so this asserts it rather
//! than assuming it. Writes go to the `proof` namespace only; no catalog table is
//! touched and no tenant data is anywhere near it.

use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use dumpster_kit_iceberg::LakeCatalogConfig;
use futures::TryStreamExt;
use iceberg::arrow::schema_to_arrow_schema;
use iceberg::spec::{DataFileFormat, NestedField, PrimitiveType, Schema, Type};
use iceberg::table::Table;
use iceberg::transaction::{ApplyTransactionAction, Transaction};
use iceberg::writer::base_writer::data_file_writer::DataFileWriterBuilder;
use iceberg::writer::file_writer::ParquetWriterBuilder;
use iceberg::writer::file_writer::location_generator::{
    DefaultFileNameGenerator, DefaultLocationGenerator,
};
use iceberg::writer::file_writer::rolling_writer::RollingFileWriterBuilder;
use iceberg::writer::{IcebergWriter, IcebergWriterBuilder};
use iceberg::{Catalog, NamespaceIdent, TableCreation, TableIdent};
use parquet::file::properties::WriterProperties;

pub const NAMESPACE: &str = "proof";
pub const TABLE: &str = "roundtrip";

pub type Row = (i64, &'static str);
pub const FIRST: &[Row] = &[(1, "first commit")];
pub const SECOND: &[Row] = &[(2, "second commit")];

pub fn schema() -> Result<Schema> {
    Ok(Schema::builder()
        .with_fields(vec![
            Arc::new(NestedField::required(
                1,
                "id",
                Type::Primitive(PrimitiveType::Long),
            )),
            Arc::new(NestedField::required(
                2,
                "note",
                Type::Primitive(PrimitiveType::String),
            )),
        ])
        .build()?)
}

fn ident() -> TableIdent {
    TableIdent::new(NamespaceIdent::new(NAMESPACE.into()), TABLE.into())
}

async fn append(cat: &dyn Catalog, table: &Table, rows: &[Row]) -> Result<Table> {
    let meta = table.metadata();
    let arrow = Arc::new(schema_to_arrow_schema(meta.current_schema())?);
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(rows.iter().map(|r| r.0))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.1))),
    ];
    let batch = RecordBatch::try_new(arrow, cols)?;
    let parquet = ParquetWriterBuilder::new(
        WriterProperties::builder().build(),
        meta.current_schema().clone(),
    );
    let rolling = RollingFileWriterBuilder::new_with_default_file_size(
        parquet,
        table.file_io().clone(),
        DefaultLocationGenerator::new(meta)?,
        DefaultFileNameGenerator::new(format!("dk-{}", uuid_v4()), None, DataFileFormat::Parquet),
    );
    let mut writer = DataFileWriterBuilder::new(rolling).build(None).await?;
    writer.write(batch).await?;
    let files = writer.close().await?;
    let tx = Transaction::new(table);
    let tx = tx.fast_append().add_data_files(files).apply(tx)?;
    Ok(tx.commit(cat).await?)
}

fn uuid_v4() -> String {
    // Unique per call is all the file name needs; avoid a dependency for it.
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("{n:x}-{:x}", std::process::id())
}

async fn rows(table: &Table) -> Result<Vec<(i64, String)>> {
    let batches: Vec<RecordBatch> = table
        .scan()
        .build()?
        .to_arrow()
        .await?
        .try_collect()
        .await?;
    let mut out = vec![];
    for b in &batches {
        let ids = b
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .context("id column")?;
        let notes = b
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .context("note column")?;
        for i in 0..b.num_rows() {
            out.push((ids.value(i), notes.value(i).to_string()));
        }
    }
    out.sort();
    Ok(out)
}

fn want(rows: &[&[Row]]) -> Vec<(i64, String)> {
    let mut v: Vec<_> = rows
        .iter()
        .flat_map(|r| r.iter())
        .map(|(i, n)| (*i, n.to_string()))
        .collect();
    v.sort();
    v
}

fn check(label: &str, got: Vec<(i64, String)>, expected: Vec<(i64, String)>) -> Result<()> {
    ensure!(
        got == expected,
        "{label}: expected {expected:?}, got {got:?}"
    );
    println!("    ok   {label}");
    Ok(())
}

/// Nessie's own v2 API base, derived from the Iceberg REST endpoint.
pub fn nessie_api_base(uri: &str) -> Result<String> {
    let base = uri.trim_end_matches('/');
    let Some(root) = base.strip_suffix("/iceberg") else {
        bail!("expected an Iceberg REST endpoint ending in /iceberg, got {uri:?}");
    };
    Ok(format!("{root}/api/v2"))
}

/// The Nessie commit hash at the tip of `branch`.
pub async fn head_hash(uri: &str, branch: &str) -> Result<String> {
    let resp = reqwest::Client::new()
        .get(format!("{}/trees/{branch}", nessie_api_base(uri)?))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?
        .error_for_status()?;
    let body: serde_json::Value = resp.json().await?;
    body["reference"]["hash"]
        .as_str()
        .map(str::to_string)
        .context("nessie answered no reference.hash")
}

pub async fn run() -> Result<()> {
    let config = LakeCatalogConfig::from_env()?;
    let cat = dumpster_kit_iceberg::open(&config).await?;
    let branch = config.reference.clone();
    println!("==> catalog: {} (ref {branch})", config.uri);
    if branch.contains('@') {
        bail!("PKDUMP_LAKE_REF must be a branch to run this proof, got {branch:?}");
    }

    // Re-runnable against a live instance, which a documented proof has to be.
    if cat.table_exists(&ident()).await? {
        cat.drop_table(&ident()).await?;
    }
    let ns = NamespaceIdent::new(NAMESPACE.into());
    if !cat.namespace_exists(&ns).await? {
        cat.create_namespace(&ns, Default::default()).await?;
    }

    println!("==> 1. create a table");
    let table = cat
        .create_table(
            &ns,
            TableCreation::builder()
                .name(TABLE.into())
                .schema(schema()?)
                .build(),
        )
        .await?;
    println!(
        "    {}.{TABLE} at {}",
        NAMESPACE,
        table.metadata().location()
    );

    println!("==> 2. write rows");
    let table = append(&cat, &table, FIRST).await?;
    let commit_one = head_hash(&config.uri, &branch).await?;
    let snapshot_one = table
        .metadata()
        .current_snapshot()
        .context("no snapshot after commit 1")?
        .snapshot_id();
    println!("    nessie commit {commit_one}");
    println!("    iceberg snapshot {snapshot_one}");

    println!("==> 3. read them back");
    check("rows after commit 1", rows(&table).await?, want(&[FIRST]))?;

    println!("==> 4. write a second commit");
    let table = append(&cat, &table, SECOND).await?;
    let commit_two = head_hash(&config.uri, &branch).await?;
    println!("    nessie commit {commit_two}");
    check(
        "rows after commit 2",
        rows(&table).await?,
        want(&[FIRST, SECOND]),
    )?;
    ensure!(
        commit_two != commit_one,
        "nessie did not advance {branch} — the two writes are one commit"
    );

    println!("==> 5. time travel");
    let past_cfg = LakeCatalogConfig {
        reference: format!("{branch}@{commit_one}"),
        ..config.clone()
    };
    let past = dumpster_kit_iceberg::open(&past_cfg).await?;
    check(
        &format!("nessie ref {branch}@<commit 1>"),
        rows(&past.load_table(&ident()).await?).await?,
        want(&[FIRST]),
    )?;
    let fresh = cat.load_table(&ident()).await?;
    check(
        &format!("{branch} is still at commit 2"),
        rows(&fresh).await?,
        want(&[FIRST, SECOND]),
    )?;

    // Measured, not asserted: what Nessie does to per-table snapshots is the
    // evidence behind "is Nessie needed yet?".
    let kept: Vec<i64> = fresh
        .metadata()
        .snapshots()
        .map(|s| s.snapshot_id())
        .collect();
    println!(
        "    note snapshots Nessie keeps in table metadata: {} {kept:?}",
        kept.len()
    );
    let travel = fresh.scan().snapshot_id(snapshot_one).build();
    match travel {
        Ok(_) => println!("    note iceberg snapshot_id time travel: AVAILABLE"),
        Err(e) => {
            println!("    note iceberg snapshot_id time travel: UNAVAILABLE under Nessie ({e})")
        }
    }

    println!("==> round trip PASSED (write, read, commit, time-travel)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_nessie_api_is_derived_from_the_iceberg_endpoint() {
        assert_eq!(
            nessie_api_base("http://nessie:19120/iceberg/").unwrap(),
            "http://nessie:19120/api/v2"
        );
        assert!(nessie_api_base("http://nessie:19120/").is_err());
    }

    #[test]
    fn the_proof_table_has_the_pythons_schema() {
        let s = schema().unwrap();
        let f: Vec<_> = s
            .as_struct()
            .fields()
            .iter()
            .map(|f| (f.id, f.name.clone(), f.required))
            .collect();
        assert_eq!(f, vec![(1, "id".into(), true), (2, "note".into(), true)]);
    }
}
