//! `pkdump import` — load a portable export back into the collection.
//!
//! `--json` restores a whole user database from an envelope written by
//! `pkdump export --json`. This is a restore, not a merge: the envelope is
//! the complete state, so every user table is replaced by its contents.

use std::path::PathBuf;

use pkdump_db::json_backup::{self, OnExisting};

/// Arguments for `pkdump import`.
#[derive(clap::Args)]
pub struct ImportArgs {
    /// Read a versioned JSON envelope written by `pkdump export --json`.
    /// Currently the only supported format.
    #[arg(long)]
    json: bool,

    /// The envelope to load.
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// Replace the existing collection. Required when the target database
    /// already holds rows — an envelope import is a whole-database restore.
    #[arg(long)]
    force: bool,
}

/// Execute `pkdump import`. The collection path comes from `PKDUMP_HOME` /
/// `PKDUMP_USER`, the same as `pkdump serve`.
pub fn run(args: ImportArgs) -> anyhow::Result<()> {
    if !args.json {
        anyhow::bail!("pass --json (the only supported import format)");
    }
    let envelope = std::fs::read_to_string(&args.file)?;
    let user_db = pkdump_db::user_db_path(&pkdump_db::current_user())?;
    let mut conn = pkdump_db::open_user(&user_db)?;

    let on_existing = if args.force {
        OnExisting::Replace
    } else {
        OnExisting::Fail
    };
    let summary = json_backup::import(&mut conn, &envelope, on_existing)?;

    println!("Imported {} -> {}", args.file.display(), user_db.display());
    for (table, rows) in &summary.tables {
        println!("  {table:<28} {rows:>7}");
    }
    println!("  {:<28} {:>7}", "total", summary.total());
    Ok(())
}
