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

    /// Directory holding the built SvelteKit SPA. Defaults to
    /// `$PKDUMP_STATIC_DIR`, else `frontend/build`.
    #[arg(long, value_name = "DIR")]
    static_dir: Option<PathBuf>,
}

/// Execute `pkdump serve`. Database paths come from `PKDUMP_HOME` /
/// `PKDUMP_USER` (the shared catalog and the active user's collection).
pub fn run(args: ServeArgs) -> anyhow::Result<()> {
    let data_dir = pkdump_db::pkdump_home()?;
    let shared_db = pkdump_db::shared_db_path()?;
    let user_db = pkdump_db::user_db_path(&pkdump_db::current_user())?;
    let static_dir = args.static_dir.unwrap_or_else(|| {
        std::env::var("PKDUMP_STATIC_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("frontend/build"))
    });
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(pkdump_server::serve(
        user_db, shared_db, static_dir, data_dir, args.host, args.port,
    ))
}
