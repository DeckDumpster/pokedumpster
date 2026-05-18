//! `pkdump` — PokeDumpster command-line entry point.
//!
//! The clap command tree grows as features land (PLAN.md §2, §5). `serve`
//! and the `ingest`/`data` subcommands arrive with later tasks.

mod setup;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "pkdump",
    version,
    about = "PokeDumpster — a Pokémon TCG collection tracker"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build the shared catalog database from upstream sources.
    Setup(setup::SetupArgs),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Setup(args) => setup::run(args),
    }
}
