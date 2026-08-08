//! `pkdump serve` — start the HTTP server.
//!
//! `setup` uses `reqwest::blocking`, which cannot run inside a Tokio
//! runtime, so the CLI's `main` stays synchronous and `serve` spins up its
//! own runtime here.

use std::net::IpAddr;
use std::path::PathBuf;

use pkdump_server::ServeConfig;

/// Environment variable form of `--multi-tenant`, for the container
/// deployment, which passes configuration as env and not as argv.
const MULTITENANT_ENV: &str = "PKDUMP_MULTITENANT";

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

    /// Serve every tenant, resolving each request to one of them by its
    /// `x-pkdump-tenant` header, instead of serving only `$PKDUMP_USER`.
    ///
    /// OFF by default and it must stay off in production: nothing
    /// authenticates the header, so with this on any caller may name any
    /// tenant. Also settable as `PKDUMP_MULTITENANT=1`. See
    /// `deploy/TENANTS.md`.
    #[arg(long)]
    multi_tenant: bool,
}

/// Execute `pkdump serve`. Database paths come from `PKDUMP_HOME` /
/// `PKDUMP_USER` (the shared catalog and the active user's collection).
pub fn run(args: ServeArgs) -> anyhow::Result<()> {
    let tenant = pkdump_db::current_user();
    // Resolved once, up front, so a data directory this process cannot make
    // sense of is a startup failure with a command in it — never a server that
    // comes up serving an empty collection. `crate::collection` also warns here
    // if the databases are still named by handle (`pkdump tenant migrate`).
    let collection = crate::collection::resolve()?;
    println!(
        "pkdump: tenant {tenant:?} -> {} ({})",
        collection.path.display(),
        crate::collection::describe(&collection)
    );
    let cfg = ServeConfig {
        user_db: collection.path,
        tenants_dir: pkdump_db::tenants_dir()?,
        registry_db: pkdump_db::registry_db_path()?,
        tenant,
        shared_db: pkdump_db::shared_db_path()?,
        data_dir: pkdump_db::pkdump_home()?,
        static_dir: args.static_dir.unwrap_or_else(|| {
            std::env::var("PKDUMP_STATIC_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("frontend/build"))
        }),
        host: args.host,
        port: args.port,
        multi_tenant: args.multi_tenant || env_opt_in(MULTITENANT_ENV),
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(pkdump_server::serve(cfg))
}

/// Whether `name` is set to an affirmative value: `1`, `true` or `yes`,
/// case-insensitively. Anything else — unset, empty, `0`, `false`, or a
/// typo — is not opting in.
///
/// Deliberately not clap's `env`, which treats a flag's variable as set by
/// its mere presence: `PKDUMP_MULTITENANT=0` reading as "on" is not a
/// surprise this particular switch can afford, since it is the only thing
/// keeping an unauthenticated resolver out of the request path.
fn env_opt_in(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_affirmative_value_opts_in() {
        // `cargo test` is threaded and the environment is process-global, so
        // this test owns one variable no other test touches.
        const VAR: &str = "PKDUMP_TEST_OPT_IN";
        for on in ["1", "true", "TRUE", "yes", "Yes"] {
            unsafe { std::env::set_var(VAR, on) };
            assert!(env_opt_in(VAR), "{on:?} should opt in");
        }
        for off in ["", "0", "false", "no", "off", "maybe"] {
            unsafe { std::env::set_var(VAR, off) };
            assert!(!env_opt_in(VAR), "{off:?} must not opt in");
        }
        unsafe { std::env::remove_var(VAR) };
        assert!(!env_opt_in(VAR), "unset must not opt in");
    }
}
