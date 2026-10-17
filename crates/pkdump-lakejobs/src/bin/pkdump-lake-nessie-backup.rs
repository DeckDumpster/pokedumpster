//! `pkdump-lake-nessie-backup` — put / check / fetch / drill / reregister /
//! inventory, the Python module's subcommands with its flags and exit statuses.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use pkdump_lakejobs::nessie_backup::{self as nb, BackupError, Store};

#[derive(Parser)]
#[command(name = "pkdump-lake-nessie-backup")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
struct StoreArg {
    /// s3://bucket/prefix (default $PKDUMP_NESSIE_BACKUP_URL; no fallback)
    #[arg(long)]
    store: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// verify an export and ship it with a table inventory
    Put {
        #[command(flatten)]
        store: StoreArg,
        #[arg(long)]
        export: PathBuf,
        /// UTC YYYYMMDDTHHMMSSZ, named by the scheduler
        #[arg(long)]
        backup_id: String,
        #[arg(long)]
        nessie_version: String,
        /// inventory JSON written by `inventory`
        #[arg(long)]
        inventory: PathBuf,
        #[arg(long, default_value_t = nb::DEFAULT_KEEP)]
        keep: usize,
    },
    /// write the live catalog's table inventory as JSON
    Inventory {
        #[arg(long)]
        out: PathBuf,
    },
    /// age + verify alarm: 0 ok, 3 stale/absent, 4 corrupt
    Check {
        #[command(flatten)]
        store: StoreArg,
        /// ISO-8601 UTC instant, named by the scheduler
        #[arg(long)]
        now: String,
        #[arg(long, default_value_t = nb::DEFAULT_MAX_AGE_HOURS)]
        max_age_hours: f64,
    },
    /// download+verify the newest backup for a restore
    Fetch {
        #[command(flatten)]
        store: StoreArg,
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// hold a restored catalog to a manifest: 0 ok, 5 on any failure
    Drill {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        require_namespace: Vec<String>,
    },
    /// fallback: re-register tables from metadata.json in S3
    Reregister {
        /// s3://bucket/prefix of the Iceberg warehouse
        #[arg(long)]
        warehouse: String,
    },
}

fn need_store(a: &StoreArg) -> Result<Box<dyn Store>, BackupError> {
    let env = std::env::var("PKDUMP_NESSIE_BACKUP_URL").ok();
    match a
        .store
        .as_deref()
        .or(env.as_deref())
        .filter(|s| !s.is_empty())
    {
        Some(url) => nb::open_store(url),
        None => Err(BackupError(
            "PKDUMP_NESSIE_BACKUP_URL is not set and --store was not given".into(),
        )),
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

fn catalog(rt: &tokio::runtime::Runtime) -> anyhow::Result<iceberg_catalog_rest::RestCatalog> {
    let config = dumpster_kit_iceberg::LakeCatalogConfig::from_env()?;
    Ok(rt.block_on(dumpster_kit_iceberg::open(&config))?)
}

fn run(cli: Cli) -> Result<i32, BackupError> {
    let any = |e: anyhow::Error| BackupError(format!("{e:#}"));
    match cli.cmd {
        Cmd::Inventory { out } => {
            let rt = runtime();
            let cat = catalog(&rt).map_err(any)?;
            let inv = rt.block_on(nb::inventory(&cat))?;
            std::fs::write(out, serde_json::to_vec_pretty(&inv).expect("serialize"))
                .map_err(|e| BackupError(e.to_string()))?;
            Ok(0)
        }
        Cmd::Put {
            store,
            export,
            backup_id,
            nessie_version,
            inventory,
            keep,
        } => {
            let read = |p: &PathBuf| {
                std::fs::read(p).map_err(|e| BackupError(format!("{}: {e}", p.display())))
            };
            let tables = serde_json::from_slice(&read(&inventory)?)
                .map_err(|e| BackupError(format!("inventory is unreadable: {e}")))?;
            let m = nb::put_backup(
                read(&export)?,
                need_store(&store)?.as_ref(),
                tables,
                &backup_id,
                &nessie_version,
                keep,
            )?;
            println!(
                "nessie-backup: {} stored, {} table(s), {} bytes",
                m.id,
                m.tables.len(),
                m.export.bytes
            );
            Ok(0)
        }
        Cmd::Check {
            store,
            now,
            max_age_hours,
        } => {
            let now: DateTime<Utc> = DateTime::parse_from_rfc3339(&now.replace(' ', "T"))
                .map_err(|e| BackupError(format!("--now {now:?}: {e}")))?
                .with_timezone(&Utc);
            let (code, why) = nb::check(need_store(&store)?.as_ref(), now, max_age_hours)?;
            if code == 0 {
                println!("nessie-backup: OK — {why}");
            } else {
                eprintln!("nessie-backup: FAILED — {why}");
            }
            Ok(code)
        }
        Cmd::Fetch { store, out_dir } => {
            let store = need_store(&store)?;
            let ids = nb::backup_ids(store.as_ref())?;
            let Some(newest) = ids.last() else {
                eprintln!("nessie-backup: no backup exists");
                return Ok(nb::EXIT_STALE);
            };
            let (m, data) = nb::verify_backup(store.as_ref(), newest)?;
            std::fs::create_dir_all(&out_dir).map_err(|e| BackupError(e.to_string()))?;
            std::fs::write(out_dir.join(nb::EXPORT), data)
                .map_err(|e| BackupError(e.to_string()))?;
            std::fs::write(out_dir.join(nb::MANIFEST), nb::to_python_json(&m))
                .map_err(|e| BackupError(e.to_string()))?;
            println!("nessie-backup: fetched {}", m.id);
            Ok(0)
        }
        Cmd::Drill {
            manifest,
            require_namespace,
        } => {
            let m: nb::Manifest = serde_json::from_slice(
                &std::fs::read(&manifest)
                    .map_err(|e| BackupError(format!("{}: {e}", manifest.display())))?,
            )
            .map_err(|e| BackupError(format!("manifest is unreadable: {e}")))?;
            let rt = runtime();
            let cat = catalog(&rt).map_err(any)?;
            let fails = rt.block_on(nb::drill(&cat, &m, &require_namespace));
            for f in &fails {
                eprintln!("drill: FAIL {f}");
            }
            if !fails.is_empty() {
                return Ok(nb::EXIT_UNREADABLE);
            }
            println!(
                "drill: OK — {} table(s) read back through the restored catalog",
                m.tables.len()
            );
            Ok(0)
        }
        Cmd::Reregister { warehouse } => {
            let store = nb::open_store(&warehouse)?;
            let found = nb::discover_tables(&store.keys("")?);
            if found.is_empty() {
                eprintln!("reregister: no metadata.json found under the warehouse");
                return Ok(1);
            }
            let rt = runtime();
            let cat = catalog(&rt).map_err(any)?;
            for t in rt.block_on(nb::reregister(&cat, &found, &warehouse))? {
                println!("reregistered {t}");
            }
            Ok(0)
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let is_put_or_fetch = matches!(cli.cmd, Cmd::Put { .. } | Cmd::Fetch { .. });
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("nessie-backup: {e}");
            std::process::exit(if is_put_or_fetch { nb::EXIT_CORRUPT } else { 1 });
        }
    }
}
