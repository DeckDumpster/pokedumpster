//! `pkdump` — PokeDumpster command-line entry point.
//!
//! The clap command tree grows as features land (PLAN.md §2, §5). The
//! `ingest` subcommand arrives with a later task.

mod collection;
mod data;
mod db;
mod export;
mod fixture;
mod import;
mod keys;
mod landing;
mod outbox;
mod serve;
mod setup;
mod tenant;

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
    /// Provision the per-tenant collection databases.
    Tenant(tenant::TenantArgs),
    /// Tenant-zone key custody: the master key, and which keys may be derived.
    Keys(keys::KeysArgs),
    /// Build the deterministic test fixture for the intents UI harness.
    SeedFixture(fixture::FixtureArgs),
    /// Database maintenance — snapshot/restore for the UI test harness.
    Db(db::DbArgs),
    /// Write the collection out in a portable format.
    Export(export::ExportArgs),
    /// Load a portable export back into the collection.
    Import(import::ImportArgs),
    /// Emit current holdings as outbox events — backfill, redrive, DR.
    Outbox(outbox::OutboxArgs),
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Setup(args) => setup::run(args),
        Command::Data(args) => data::run(args),
        Command::Serve(args) => serve::run(args),
        Command::Tenant(args) => tenant::run(args),
        Command::Keys(args) => keys::run(args),
        Command::SeedFixture(args) => fixture::run(args),
        Command::Db(args) => db::run(args),
        Command::Export(args) => export::run(args),
        Command::Import(args) => import::run(args),
        Command::Outbox(args) => outbox::run(args),
    }
}
