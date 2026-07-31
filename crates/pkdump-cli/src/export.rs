//! `pkdump export` — write the collection out in a portable format.
//!
//! `--json` dumps every user table into one versioned envelope
//! (`pkdump_db::json_backup`): a human-inspectable backup that
//! `pkdump import --json` restores exactly. The shared catalog is not
//! included — it is reproducible from upstream with `pkdump setup`.

use std::path::PathBuf;

/// Arguments for `pkdump export`.
#[derive(clap::Args)]
pub struct ExportArgs {
    /// Write the whole user database as a versioned JSON envelope.
    /// Currently the only supported format.
    #[arg(long)]
    json: bool,

    /// File to write. Defaults to stdout.
    #[arg(long, short, value_name = "FILE")]
    out: Option<PathBuf>,
}

/// Execute `pkdump export`. The collection path comes from `PKDUMP_HOME` /
/// `PKDUMP_USER`, the same as `pkdump serve`.
pub fn run(args: ExportArgs) -> anyhow::Result<()> {
    if !args.json {
        anyhow::bail!("pass --json (the only supported export format)");
    }
    let user_db = pkdump_db::user_db_path(&pkdump_db::current_user())?;
    let conn = pkdump_db::open_user(&user_db)?;
    let json = pkdump_db::json_backup::export(&conn)?;

    match args.out {
        // The envelope goes to stdout, so progress goes to stderr.
        Some(path) => {
            std::fs::write(&path, json.as_bytes())?;
            eprintln!("Exported {} -> {}", user_db.display(), path.display());
        }
        None => println!("{json}"),
    }
    Ok(())
}
