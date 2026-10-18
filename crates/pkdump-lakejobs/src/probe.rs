//! `pkdump-lake-probe` — read-only questions about the lake, for the container
//! gates (sp-4k7mxi.7).
//!
//! The gates used to ask these through `python -c` in the PyIceberg job image.
//! With that image gone, the answers come from the same Iceberg client the jobs
//! use, printed as plain text a shell can compare: one row per line, columns
//! tab-separated, sorted. Only `create-tables` writes (two empty tables, for a gate's fresh
//! lake), and nothing runs on a timer.

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use arrow_array::RecordBatch;
use arrow_cast::display::array_value_to_string;
use dumpster_kit_iceberg::LakeCatalogConfig;
use dumpster_kit_iceberg::prices::{
    NAMESPACE, prices_ident, prices_schema, sealed_ident, sealed_schema,
};
use futures::TryStreamExt;
use iceberg::spec::{Transform, UnboundPartitionField, UnboundPartitionSpec};
use iceberg::{Catalog, NamespaceIdent, TableCreation};

use crate::freshness::parse_ident;

async fn catalog() -> Result<impl Catalog> {
    let config = LakeCatalogConfig::from_env()?;
    Ok(dumpster_kit_iceberg::open(&config).await?)
}

/// Every `namespace.table` in the catalog, sorted, space-separated.
pub async fn tables() -> Result<String> {
    let cat = catalog().await?;
    let mut out = vec![];
    for ns in cat.list_namespaces(None).await? {
        for t in cat.list_tables(&ns).await? {
            out.push(format!("{}.{}", ns.join("."), t.name()));
        }
    }
    out.sort();
    Ok(out.join(" "))
}

/// A property of the table's current snapshot summary.
pub async fn summary(table: &str, key: &str) -> Result<String> {
    let cat = catalog().await?;
    let t = cat.load_table(&parse_ident(table)).await?;
    let snap = t
        .metadata()
        .current_snapshot()
        .with_context(|| format!("{table} has no current snapshot"))?;
    snap.summary()
        .additional_properties
        .get(key)
        .cloned()
        .with_context(|| format!("{table}'s current snapshot has no property {key}"))
}

/// The rows of `table` whose displayed column values equal every `col=value` in
/// `filters`, projected to `columns` (all when empty), tab-joined and sorted.
pub async fn rows(table: &str, filters: &[String], columns: &[String]) -> Result<Vec<String>> {
    let cat = catalog().await?;
    let t = cat.load_table(&parse_ident(table)).await?;
    let batches: Vec<RecordBatch> = t.scan().build()?.to_arrow().await?.try_collect().await?;
    let wanted: Vec<(&str, &str)> = filters
        .iter()
        .map(|f| {
            f.split_once('=')
                .with_context(|| format!("--where {f:?} is not col=value"))
        })
        .collect::<Result<_>>()?;
    let mut out = vec![];
    for b in &batches {
        let schema = b.schema();
        let idx = |name: &str| -> Result<usize> {
            schema
                .index_of(name)
                .with_context(|| format!("{table} has no column {name}"))
        };
        let show: Vec<usize> = if columns.is_empty() {
            (0..schema.fields().len()).collect()
        } else {
            columns.iter().map(|c| idx(c)).collect::<Result<_>>()?
        };
        let keys: Vec<(usize, &str)> = wanted
            .iter()
            .map(|(c, v)| Ok((idx(c)?, *v)))
            .collect::<Result<_>>()?;
        for row in 0..b.num_rows() {
            let mut keep = true;
            for (c, v) in &keys {
                if array_value_to_string(b.column(*c), row)? != *v {
                    keep = false;
                    break;
                }
            }
            if !keep {
                continue;
            }
            let cells: Vec<String> = show
                .iter()
                .map(|c| array_value_to_string(b.column(*c), row).map_err(Into::into))
                .collect::<Result<_>>()?;
            out.push(cells.join("\t"));
        }
    }
    out.sort();
    Ok(out)
}

/// Does `url` answer with a 2xx inside five seconds?
pub async fn get(url: &str) -> Result<()> {
    let r = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?;
    r.bytes().await?;
    Ok(())
}

/// `REACHABLE` or `blocked` for a TCP connect to `addr` (host:port).
pub fn reachable(addr: &str) -> Result<&'static str> {
    let Some(sock) = addr
        .to_socket_addrs()
        .with_context(|| format!("{addr} is not host:port"))?
        .next()
    else {
        bail!("{addr} resolved to nothing");
    };
    Ok(
        if TcpStream::connect_timeout(&sock, Duration::from_secs(5)).is_ok() {
            "REACHABLE"
        } else {
            "blocked"
        },
    )
}

/// Create `catalog.prices` and `catalog.sealed_prices` when absent, with the
/// partition field id written out (1000) — the one thing a gate cannot leave to
/// the kit's `ensure_tables`, which sends the partition field without an id and
/// is refused by Nessie 0.104.3 ("some of required attributes are not set
/// [fieldId]"). Production's tables already exist, so only a fresh lake — every
/// container gate — reaches the refusal. Not a probe in the read-only sense:
/// it writes the two empty tables and nothing else.
pub async fn create_tables() -> Result<()> {
    let cat = catalog().await?;
    let ns = NamespaceIdent::new(NAMESPACE.into());
    if !cat.namespace_exists(&ns).await? {
        cat.create_namespace(&ns, Default::default()).await?;
    }
    for (ident, schema, observed_id) in [
        (prices_ident(), prices_schema()?, 5),
        (sealed_ident(), sealed_schema()?, 4),
    ] {
        if cat.table_exists(&ident).await? {
            continue;
        }
        let spec = UnboundPartitionSpec::builder()
            .with_spec_id(0)
            .add_partition_fields([UnboundPartitionField {
                source_id: observed_id,
                field_id: Some(1000),
                name: "observed_date".into(),
                transform: Transform::Identity,
            }])?
            .build();
        let creation = TableCreation::builder()
            .name(ident.name().to_string())
            .schema(schema)
            .partition_spec(spec)
            .build();
        cat.create_table(&ns, creation).await?;
    }
    Ok(())
}
