use clap::Parser;
use pkdump_value_snapshots::{
    Args, DEFAULT_SEALED_TABLE, DEFAULT_TABLE, EXIT_FAILED, data_dir, report, run,
};

#[derive(Parser)]
#[command(
    name = "pkdump-value-snapshots",
    about = "Per-tenant collection value snapshots from catalog.prices. Every tenant."
)]
struct Cli {
    /// The snapshot date, YYYY-MM-DD. Required, and never defaulted from the
    /// clock: backfilling an older date is the same operation as taking today's.
    #[arg(long)]
    date: String,
    /// The pkdump data directory (default: $PKDUMP_HOME, then ~/.pkdump)
    #[arg(long)]
    data_dir: Option<String>,
    #[arg(long, default_value = DEFAULT_TABLE)]
    table: String,
    /// The sealed price feed
    #[arg(long, default_value = DEFAULT_SEALED_TABLE)]
    sealed_table: String,
    /// The Nessie ref to read (default: $PKDUMP_LAKE_REF, then main). A bare
    /// branch is pinned to its current commit for the whole run; pass
    /// 'main@<hash>' to pin it yourself.
    #[arg(long = "ref")]
    reference: Option<String>,
    /// Snapshot only this tenant (repeatable). The default is every active
    /// tenant, which is the point of the job.
    #[arg(long = "tenant", value_name = "HANDLE")]
    tenant: Vec<String>,
    /// Compute everything, write nothing
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if chrono::NaiveDate::parse_from_str(&cli.date, "%Y-%m-%d").is_err() {
        eprintln!("!! --date {:?} is not YYYY-MM-DD", cli.date);
        std::process::exit(EXIT_FAILED);
    }
    let args = Args {
        root: data_dir(cli.data_dir.as_deref()),
        date: cli.date,
        table: cli.table,
        sealed_table: cli.sealed_table,
        reference: cli.reference,
        only: (!cli.tenant.is_empty()).then_some(cli.tenant),
        dry_run: cli.dry_run,
    };
    match run(&args).await {
        Ok(outcomes) => std::process::exit(report(&outcomes, &args.date)),
        Err(e) => {
            eprintln!("!! {e}");
            std::process::exit(EXIT_FAILED);
        }
    }
}
