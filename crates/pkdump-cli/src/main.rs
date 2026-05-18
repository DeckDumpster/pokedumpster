//! `pkdump` — PokeDumpster command-line entry point.
//!
//! The clap command tree (`setup`, `data refresh`, `serve`, `ingest …`) is
//! added by later M1/M2 tasks (PLAN.md §2, §5). For now this is a skeleton
//! that confirms the workspace links and runs.

fn main() {
    println!("pkdump {}", pkdump_core::VERSION);
}
