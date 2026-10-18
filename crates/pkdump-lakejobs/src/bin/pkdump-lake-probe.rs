//! `pkdump-lake-probe` — read-only questions about the lake, for the container gates.

use clap::{Parser, Subcommand};
use pkdump_lakejobs::probe;

#[derive(Parser)]
#[command(name = "pkdump-lake-probe")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Every namespace.table, sorted, on one line
    Tables,
    /// A property of a table's current snapshot summary
    Summary {
        #[arg(long)]
        table: String,
        #[arg(long)]
        key: String,
    },
    /// Rows as tab-separated, sorted lines
    Rows {
        #[arg(long)]
        table: String,
        /// col=value (the column's displayed text); repeatable
        #[arg(long = "where")]
        filters: Vec<String>,
        /// Columns to print, comma-separated (default all)
        #[arg(long, value_delimiter = ',')]
        columns: Vec<String>,
    },
    /// Exit 0 when the URL answers 2xx
    Get { url: String },
    /// Print REACHABLE or blocked for a TCP connect to host:port
    Reachable { addr: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Args::parse().cmd {
        Cmd::Tables => println!("{}", probe::tables().await?),
        Cmd::Summary { table, key } => println!("{}", probe::summary(&table, &key).await?),
        Cmd::Rows {
            table,
            filters,
            columns,
        } => {
            for line in probe::rows(&table, &filters, &columns).await? {
                println!("{line}");
            }
        }
        Cmd::Get { url } => probe::get(&url).await?,
        Cmd::Reachable { addr } => println!("{}", probe::reachable(&addr)?),
    }
    Ok(())
}
