//! `pkdump serve` — start the HTTP server.
//!
//! `setup` uses `reqwest::blocking`, which cannot run inside a Tokio
//! runtime, so the CLI's `main` stays synchronous and `serve` spins up its
//! own runtime here.

use std::net::IpAddr;
use std::path::PathBuf;

/// Arguments for `pkdump serve`.
#[derive(clap::Args)]
pub struct ServeArgs {
    /// Address to bind. Defaults to localhost; the container deployment
    /// passes `0.0.0.0`.
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,

    /// Port to listen on.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Shared catalog database path (default: ~/.pkdump/shared.sqlite).
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
}

/// Execute `pkdump serve`.
pub fn run(args: ServeArgs) -> anyhow::Result<()> {
    let db_path = match args.db {
        Some(p) => p,
        None => pkdump_db::shared_db_path()?,
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(pkdump_server::serve(db_path, args.host, args.port))
}
