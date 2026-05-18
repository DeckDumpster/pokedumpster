//! `pkdump-db` — SQLite persistence for PokeDumpster.
//!
//! Owns the shared/user database split (PLAN.md §3.1): a read-only shared
//! catalog DB `ATTACH`ed to a mutable per-user collection DB. `refinery`
//! migrations and the repository layer are added by later M1/M2 tasks.
