//! Replace one `observed_date` of an Iceberg table in a single commit.
//!
//! iceberg-rust 0.10 can only `fast_append`, and its `TableCommit` cannot be built outside the
//! crate, so the Python lake's `table.overwrite(overwrite_filter=observed_date == D)` has no
//! action to call. Everything an overwrite needs IS public, though: data files, manifests, the
//! manifest list, and the `TableUpdate` / `TableRequirement` wire types. So the snapshot is built
//! here, as `SnapshotProducer` would, and committed with the Iceberg REST `updateTable` call the
//! client itself makes.
//!
//! The tables are identity-partitioned on `observed_date`, so a data file is wholly inside or
//! wholly outside the day being replaced. That is what makes the removal exact: every live entry
//! whose partition is the day is rewritten as DELETED, every other entry carries over EXISTING,
//! and a manifest that holds nothing of the day is reused untouched. One snapshot, operation
//! `overwrite`, carries both the removal and the new rows — the day is never visibly absent or
//! doubled.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, anyhow, bail};
use arrow_array::{ArrayRef, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray};
use dumpster_kit_iceberg::{LakeCatalogConfig, prices};
use iceberg::arrow::schema_to_arrow_schema;
use iceberg::spec::{
    DataFileFormat, Datum, FormatVersion, Literal, MAIN_BRANCH, ManifestContentType, ManifestFile,
    ManifestList, ManifestListWriter, ManifestWriterBuilder, Operation, PartitionKey,
    PartitionSpec, Snapshot, SnapshotReference, SnapshotRetention, Struct, Summary,
};
use iceberg::table::Table;
use iceberg::writer::base_writer::data_file_writer::DataFileWriterBuilder;
use iceberg::writer::file_writer::ParquetWriterBuilder;
use iceberg::writer::file_writer::location_generator::{
    DefaultFileNameGenerator, DefaultLocationGenerator,
};
use iceberg::writer::file_writer::rolling_writer::RollingFileWriterBuilder;
use iceberg::writer::{IcebergWriter, IcebergWriterBuilder};
use iceberg::{TableRequirement, TableUpdate};
use parquet::file::properties::WriterProperties;

pub fn prices_batch(
    table: &Table,
    rows: &[prices::PriceRow],
    days: i32,
) -> anyhow::Result<RecordBatch> {
    let schema = Arc::new(schema_to_arrow_schema(table.metadata().current_schema())?);
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.tcgplayer_product_id),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.sub_type_name.as_str()),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.price_type.as_str()),
        )),
        Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.price))),
        Arc::new(Date32Array::from(vec![days; rows.len()])),
    ];
    Ok(RecordBatch::try_new(schema, cols)?)
}

pub fn sealed_batch(
    table: &Table,
    rows: &[prices::SealedPriceRow],
    days: i32,
) -> anyhow::Result<RecordBatch> {
    let schema = Arc::new(schema_to_arrow_schema(table.metadata().current_schema())?);
    let cols: Vec<ArrayRef> = vec![
        Arc::new(Int64Array::from_iter_values(
            rows.iter().map(|r| r.tcgplayer_product_id),
        )),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.price_type.as_str()),
        )),
        Arc::new(Float64Array::from_iter_values(rows.iter().map(|r| r.price))),
        Arc::new(Date32Array::from(vec![days; rows.len()])),
    ];
    Ok(RecordBatch::try_new(schema, cols)?)
}

/// Days since the epoch of `YYYY-MM-DD`, as an Iceberg date.
pub fn days_since_epoch(date: &str) -> anyhow::Result<i32> {
    match Datum::date_from_str(date)?.literal() {
        iceberg::spec::PrimitiveLiteral::Int(d) => Ok(*d),
        other => bail!("date literal was {other:?}"),
    }
}

/// Replace `observed_date` in `table` with `batch`, as one commit. Returns the rows replaced.
pub async fn replace_day(
    config: &LakeCatalogConfig,
    table: &Table,
    days: i32,
    batch: RecordBatch,
    provenance: HashMap<String, String>,
) -> anyhow::Result<usize> {
    let (requirements, updates, removed) = plan(table, days, batch, provenance).await?;
    commit(config, table, requirements, updates).await?;
    Ok(removed)
}

type Plan = (Vec<TableRequirement>, Vec<TableUpdate>, usize);

/// Write the data file and manifests and return the commit that makes them current.
async fn plan(
    table: &Table,
    days: i32,
    batch: RecordBatch,
    provenance: HashMap<String, String>,
) -> anyhow::Result<Plan> {
    let meta = table.metadata();
    let file_io = table.file_io();
    let snapshot_id = new_snapshot_id(table);
    let commit_uuid = uuid::Uuid::new_v4();
    let seq = meta.next_sequence_number();
    let parent = meta.current_snapshot_id();
    let schema = meta.current_schema().clone();
    let spec: PartitionSpec = meta.default_partition_spec().as_ref().clone();
    let day = Struct::from_iter([Some(Literal::date(days))]);
    let format = meta.format_version();
    if format != FormatVersion::V2 {
        bail!(
            "{}: replace is written for format v2, table is {format:?}",
            table.identifier().name()
        );
    }

    let mut manifests: Vec<ManifestFile> = Vec::new();
    let mut counter = 0usize;
    let mut removed_records = 0u64;
    let mut removed_files = 0u64;
    if let Some(current) = meta.current_snapshot() {
        let bytes = file_io.new_input(current.manifest_list())?.read().await?;
        let list = ManifestList::parse_with_version(&bytes, format)?;
        for mf in list.entries() {
            if mf.content != ManifestContentType::Data {
                manifests.push(mf.clone());
                continue;
            }
            let manifest = mf.load_manifest(file_io).await?;
            let in_day = |e: &Arc<iceberg::spec::ManifestEntry>| {
                e.is_alive() && e.data_file().partition() == &day
            };
            if !manifest.entries().iter().any(in_day) {
                manifests.push(mf.clone());
                continue;
            }
            counter += 1;
            let out = file_io.new_output(format!(
                "{}/metadata/{commit_uuid}-m{counter}.avro",
                meta.location()
            ))?;
            let mut writer =
                ManifestWriterBuilder::new(out, Some(snapshot_id), schema.clone(), spec.clone())
                    .build_v2_data();
            for e in manifest.entries() {
                if !e.is_alive() {
                    continue;
                }
                let data_seq = e.sequence_number().ok_or_else(|| {
                    anyhow!("{}: entry without a sequence number", mf.manifest_path)
                })?;
                let file = e.data_file().clone();
                if in_day(e) {
                    removed_records += file.record_count();
                    removed_files += 1;
                    writer.add_delete_file(file, data_seq, Some(data_seq))?;
                } else {
                    let from = e.snapshot_id().unwrap_or(mf.added_snapshot_id);
                    writer.add_existing_file(file, from, data_seq, Some(data_seq))?;
                }
            }
            manifests.push(writer.write_manifest_file().await?);
        }
    }

    let added_records = batch.num_rows() as u64;
    let mut added_files = 0u64;
    if batch.num_rows() > 0 {
        let key = PartitionKey::new(spec.clone(), schema.clone(), day.clone());
        let location = DefaultLocationGenerator::new(meta)?;
        let names = DefaultFileNameGenerator::new(
            format!("dk-{}", uuid::Uuid::new_v4()),
            None,
            DataFileFormat::Parquet,
        );
        let parquet =
            ParquetWriterBuilder::new(WriterProperties::builder().build(), schema.clone());
        let rolling = RollingFileWriterBuilder::new_with_default_file_size(
            parquet,
            file_io.clone(),
            location,
            names,
        );
        let mut writer = DataFileWriterBuilder::new(rolling).build(Some(key)).await?;
        writer.write(batch).await?;
        let files = writer.close().await?;
        added_files = files.len() as u64;
        counter += 1;
        let out = file_io.new_output(format!(
            "{}/metadata/{commit_uuid}-m{counter}.avro",
            meta.location()
        ))?;
        let mut mw =
            ManifestWriterBuilder::new(out, Some(snapshot_id), schema.clone(), spec.clone())
                .build_v2_data();
        for f in files {
            mw.add_file(f, seq)?;
        }
        manifests.push(mw.write_manifest_file().await?);
    }

    let list_path = format!(
        "{}/metadata/snap-{snapshot_id}-0-{commit_uuid}.avro",
        meta.location()
    );
    let mut list_writer = ManifestListWriter::v2(
        file_io.new_output(list_path.clone())?.writer().await?,
        snapshot_id,
        parent,
        seq,
    );
    list_writer.add_manifests(manifests.into_iter())?;
    list_writer.close().await?;

    let mut props = provenance;
    props.insert("added-data-files".into(), added_files.to_string());
    props.insert("added-records".into(), added_records.to_string());
    props.insert("deleted-data-files".into(), removed_files.to_string());
    props.insert("deleted-records".into(), removed_records.to_string());
    let snapshot = Snapshot::builder()
        .with_manifest_list(list_path)
        .with_snapshot_id(snapshot_id)
        .with_parent_snapshot_id(parent)
        .with_sequence_number(seq)
        .with_summary(Summary {
            operation: Operation::Overwrite,
            additional_properties: props,
        })
        .with_schema_id(meta.current_schema_id())
        .with_timestamp_ms(chrono::Utc::now().timestamp_millis())
        .build();
    let updates = vec![
        TableUpdate::AddSnapshot { snapshot },
        TableUpdate::SetSnapshotRef {
            ref_name: MAIN_BRANCH.to_string(),
            reference: SnapshotReference::new(
                snapshot_id,
                SnapshotRetention::branch(None, None, None),
            ),
        },
    ];
    let requirements = vec![
        TableRequirement::UuidMatch { uuid: meta.uuid() },
        TableRequirement::RefSnapshotIdMatch {
            r#ref: MAIN_BRANCH.to_string(),
            snapshot_id: parent,
        },
    ];
    Ok((requirements, updates, removed_records as usize))
}

fn new_snapshot_id(table: &Table) -> i64 {
    loop {
        let (a, b) = uuid::Uuid::new_v4().as_u64_pair();
        let id = ((a ^ b) & (i64::MAX as u64)) as i64;
        if id != 0 && table.metadata().snapshot_by_id(id).is_none() {
            return id;
        }
    }
}

/// The REST `updateTable` call: `POST {uri}/v1/{ref}/namespaces/{ns}/tables/{name}`.
async fn commit(
    config: &LakeCatalogConfig,
    table: &Table,
    requirements: Vec<TableRequirement>,
    updates: Vec<TableUpdate>,
) -> anyhow::Result<()> {
    let ident = table.identifier();
    let ns = ident.namespace().as_ref().join("\u{1f}");
    let url = format!(
        "{}/v1/{}/namespaces/{}/tables/{}",
        config.uri.trim_end_matches('/'),
        config.reference,
        ns,
        ident.name()
    );
    let body = serde_json::json!({
        "identifier": { "namespace": ident.namespace().as_ref(), "name": ident.name() },
        "requirements": requirements,
        "updates": updates,
    });
    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!(
            "POST {url}: {status}: {}",
            resp.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::TryStreamExt;
    use iceberg::io::FileIO;
    use iceberg::spec::{TableMetadataBuilder, Transform, UnboundPartitionSpec};
    use iceberg::{NamespaceIdent, TableCreation, TableIdent};

    fn row(id: i64, price: f64) -> prices::PriceRow {
        prices::PriceRow {
            tcgplayer_product_id: id,
            sub_type_name: "Normal".into(),
            price_type: "market".into(),
            price,
        }
    }

    fn empty_table(dir: &std::path::Path) -> Table {
        let spec = UnboundPartitionSpec::builder()
            .add_partition_field(5, "observed_date", Transform::Identity)
            .unwrap()
            .build();
        let creation = TableCreation::builder()
            .name("prices".into())
            .location(dir.to_str().unwrap().into())
            .schema(prices::prices_schema().unwrap())
            .partition_spec(spec)
            .build();
        let meta = TableMetadataBuilder::from_table_creation(creation)
            .unwrap()
            .build()
            .unwrap()
            .metadata;
        Table::builder()
            .metadata(meta)
            .identifier(TableIdent::new(
                NamespaceIdent::new("catalog".into()),
                "prices".into(),
            ))
            .file_io(FileIO::new_with_fs())
            .runtime(iceberg::Runtime::current())
            .build()
            .unwrap()
    }

    /// What the catalog does on commit: apply the updates to the metadata.
    fn apply(table: &Table, requirements: &[TableRequirement], updates: Vec<TableUpdate>) -> Table {
        for r in requirements {
            r.check(Some(table.metadata())).unwrap();
        }
        let mut b = TableMetadataBuilder::new_from_metadata(table.metadata().clone(), None);
        for u in updates {
            b = u.apply(b).unwrap();
        }
        Table::builder()
            .metadata(b.build().unwrap().metadata)
            .identifier(table.identifier().clone())
            .file_io(table.file_io().clone())
            .runtime(iceberg::Runtime::current())
            .build()
            .unwrap()
    }

    async fn put(table: Table, date: &str, rows: &[prices::PriceRow]) -> (Table, usize) {
        let days = days_since_epoch(date).unwrap();
        let batch = prices_batch(&table, rows, days).unwrap();
        let (req, up, removed) = plan(
            &table,
            days,
            batch,
            HashMap::from([("k".into(), date.into())]),
        )
        .await
        .unwrap();
        (apply(&table, &req, up), removed)
    }

    async fn day(table: &Table, date: &str) -> Vec<(i64, f64)> {
        use arrow_array::cast::AsArray;
        use arrow_array::types::{Float64Type, Int64Type};
        let scan = table
            .scan()
            .with_filter(
                iceberg::expr::Reference::new("observed_date")
                    .equal_to(Datum::date_from_str(date).unwrap()),
            )
            .build()
            .unwrap();
        let batches: Vec<RecordBatch> = scan.to_arrow().await.unwrap().try_collect().await.unwrap();
        let mut out = Vec::new();
        for b in &batches {
            let ids = b
                .column_by_name("tcgplayer_product_id")
                .unwrap()
                .as_primitive::<Int64Type>();
            let px = b
                .column_by_name("price")
                .unwrap()
                .as_primitive::<Float64Type>();
            for i in 0..b.num_rows() {
                out.push((ids.value(i), px.value(i)));
            }
        }
        out.sort_by(|a, b| a.partial_cmp(b).unwrap());
        out
    }

    #[tokio::test]
    async fn replacing_a_day_leaves_exactly_the_second_build_and_the_other_days() {
        let dir = tempfile::tempdir().unwrap();
        let t = empty_table(dir.path());
        let (t, removed) = put(t, "2026-08-08", &[row(1, 1.0), row(2, 2.0), row(3, 3.0)]).await;
        assert_eq!(removed, 0);
        let (t, _) = put(t, "2026-08-09", &[row(1, 9.0)]).await;

        let (t, removed) = put(t, "2026-08-08", &[row(2, 20.0), row(4, 40.0)]).await;
        assert_eq!(removed, 3);
        assert_eq!(day(&t, "2026-08-08").await, vec![(2, 20.0), (4, 40.0)]);
        assert_eq!(day(&t, "2026-08-09").await, vec![(1, 9.0)]);

        // the same input again is a no-op in content, and the history keeps growing
        let (t, removed) = put(t, "2026-08-08", &[row(2, 20.0), row(4, 40.0)]).await;
        assert_eq!(removed, 2);
        assert_eq!(day(&t, "2026-08-08").await, vec![(2, 20.0), (4, 40.0)]);
        assert_eq!(t.metadata().snapshots().count(), 4);
        let snap = t.metadata().current_snapshot().unwrap();
        assert_eq!(snap.summary().operation, Operation::Overwrite);
    }
}
