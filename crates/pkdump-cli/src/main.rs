//! `pkdump` — PokeDumpster command-line entry point.
//!
//! The clap command tree grows as features land (PLAN.md §2, §5). The
//! `ingest` subcommand arrives with a later task.

mod data;
mod fixture;
mod serve;
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
    /// Incremental catalog maintenance (nightly refresh).
    Data(data::DataArgs),
    /// Start the HTTP server.
    Serve(serve::ServeArgs),
    /// Build the deterministic test fixture for the intents UI harness.
    SeedFixture(fixture::FixtureArgs),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Setup(args) => setup::run(args),
        Command::Data(args) => data::run(args),
        Command::Serve(args) => serve::run(args),
        Command::SeedFixture(args) => fixture::run(args),
    }
}
