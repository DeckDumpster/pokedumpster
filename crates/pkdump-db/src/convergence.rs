//! The catalog's convergence fingerprint — one hash over everything a build
//! writes into `shared.sqlite` out of its own embedded inputs.
//!
//! ## The two kinds of catalog writer, and why nothing told them apart
//!
//! `shared.sqlite` has writers of two kinds. One **builds** it out of upstream
//! bytes: `pkdump setup` by hand, and nightly `pkdump-lake-derive shared`,
//! which since pd-lunn is the only thing on a box that does. The other
//! **converges** it: every [`crate::open_shared`] re-applies the schema, adds
//! any column the file predates, and re-seeds every shipped seed file, so a
//! binary upgrade can ship a data-only migration without a migration history.
//!
//! `pkdump serve` is only ever the second kind — it opens the catalog
//! read-write at startup, converges, reads the search registry off it and
//! drops the handle before the first request. That is deliberate. What was
//! not deliberate is that it did it **unconditionally**:
//! [`crate::search_meta::reconcile`] alone is a `DELETE` and a few hundred
//! `INSERT`s, so *every restart took the catalog's write lock* whether or not
//! the binary shipped anything new.
//!
//! The nightly derive holds that same lock, in transactions of its own, for
//! minutes. Two writers, five seconds of `busy_timeout` each, and nothing
//! between them (pd-dzu5): a deploy or a reboot landing inside the 07:00
//! derive fails to start with `database is locked`, and `Restart=on-failure`
//! then retries it every fifteen seconds until the build is over. Not
//! corruption — SQLite's locking is what rules that out — but the site is
//! down for the remainder of the derive, and `deploy/pkdump.container`
//! carries no `OnFailure=`, so nothing says so.
//!
//! ## What the fingerprint is
//!
//! A SHA-256 over the exact bytes of every embedded input the convergence
//! writes from, plus the schema version it stamps. A catalog carrying this
//! build's fingerprint has already had this build's convergence applied to
//! it, so a second application has nothing to do.
//!
//! Which makes the question askable **read-only**:
//! [`crate::open_shared_for_serving`] opens the catalog read-only, reads one
//! row, and on a match never opens it read-write at all. An ordinary restart
//! therefore takes no write lock and cannot race the derive. A binary that
//! genuinely ships a data-only migration still writes — that is the one case
//! the read-write open exists for, and it becomes the only case that can
//! collide.
//!
//! Four things about it are decisions:
//!
//! - **It hashes the INPUTS, never the tables.** What the convergence would
//!   write is a property of the binary; what the catalog holds is 12M price
//!   rows this has no business reading. Hashing the inputs is O(a few hundred
//!   KB of `include_str!`) and is what makes the question cheap enough to ask
//!   on the read-only path first.
//! - **It is written LAST**, after the seeds and after the version stamp, in
//!   [`crate::open_shared`] and nowhere else. A fingerprint recorded before
//!   the work would let a convergence that died halfway be skipped forever.
//! - **Anything that is not a clear YES is a NO.** An older catalog has no
//!   such table, a restored one may have no row, and a WAL database whose
//!   `-shm` is absent cannot be opened read-only at all. Every one of those
//!   answers "not converged", so the fall-through is to do the work — the
//!   direction in which being wrong costs a write lock rather than a missed
//!   migration.
//! - **The input list is asserted over the TREE, not maintained.**
//!   `every_catalog_seed_is_in_the_fingerprint` reads every `include_str!` in
//!   this crate's source and requires each to be classified into exactly one
//!   of `inputs()` and `NOT_CATALOG_INPUTS`. A seed added to the catalog
//!   convergence and forgotten here is the only way this can be wrong, and it
//!   would be silent: the fingerprint would go on matching while the new seed
//!   never reached a running server.

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::error::Result;

/// Every embedded input the shared-catalog convergence writes from, by its
/// repo-relative path.
///
/// The path is not decoration: it is what
/// `every_catalog_seed_is_in_the_fingerprint` matches against the
/// `include_str!` calls in this crate, and it is what makes a hash mismatch
/// legible to whoever has to read one.
///
/// Order is fixed and part of the hash. Reordering the list changes every
/// catalog's fingerprint once, which converges on the next open and is
/// harmless; it is called out only so nobody reads a changed hash as a bug.
pub(crate) fn inputs() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "crates/pkdump-db/src/schema_shared.sql",
            crate::connection::SCHEMA_SHARED,
        ),
        ("data/variants.json", crate::variants::VARIANTS_SEED),
        (
            "data/tcgcsv_sub_type_variants.json",
            crate::sub_type_map::SUB_TYPE_VARIANTS_SEED,
        ),
        ("data/bundles.json", crate::bundles::BUNDLES_SEED),
        (
            "data/set_aliases.json",
            crate::set_aliases::SET_ALIASES_SEED,
        ),
        (
            "data/overrides/catalog_prices.json",
            crate::catalog_prices::CATALOG_PRICES_SEED,
        ),
        (
            "data/search_keywords.json",
            crate::search_meta::KEYWORDS_SEED,
        ),
        ("data/rarities.json", crate::search_meta::RARITIES_SEED),
        ("data/search_flags.json", crate::search_meta::FLAGS_SEED),
    ]
}

/// The `include_str!`ed files in this crate that are deliberately NOT part of
/// the shared catalog's convergence, each with the reason.
///
/// This exists so the drift gate can require every embedded input to be
/// **classified** rather than merely absent. A per-file exemption list is
/// what erodes; a classification that has to cover the crate cannot be added
/// to silently — the same argument `tests/lake/tenant_isolation_test.sh` §12
/// makes about the lake's zones.
///
/// Nothing but that gate reads it, so it is compiled only for tests. It is
/// declared here rather than inside the test module because it is a statement
/// about the crate, and the next person to add an `include_str!` should meet
/// it beside the list they might otherwise have added to.
#[cfg(test)]
pub(crate) const NOT_CATALOG_INPUTS: &[(&str, &str)] = &[
    (
        "crates/pkdump-db/src/schema_user.sql",
        "the per-tenant collection database, converged by open_user on its own file",
    ),
    (
        "crates/pkdump-db/src/schema_registry.sql",
        "registry.sqlite, converged by open_registry on its own file",
    ),
    (
        "data/conditions.json",
        "the collection's own condition multipliers — pd-s4c2 moved `conditions` out of \
         the catalog and into the tenant database, and `init_user_schema` seeds it there \
         once, never overwriting what a collection already holds",
    ),
];

/// The fingerprint of the convergence THIS BUILD performs.
///
/// Length-prefixed per input, so two seeds cannot be concatenated into a
/// third that hashes the same — a seed ending where the next begins is not a
/// hypothetical when every one of them is JSON.
pub(crate) fn fingerprint() -> String {
    let mut h = Sha256::new();
    // The version is stamped by the same convergence and is not any file's
    // contents, so it is hashed in its own right. A version bump that
    // transformed rows without touching a seed would otherwise be invisible
    // here — which is precisely the change that must not be skipped.
    h.update(b"schema_version=");
    h.update(
        crate::schema_version::Database::Shared
            .version()
            .to_string()
            .as_bytes(),
    );
    h.update(b"\n");
    // The ALTER TABLE statements a catalog older than a column grows on open.
    // They live in connection.rs rather than in schema_shared.sql, so hashing
    // the schema file alone would miss them.
    for (table, column, ddl) in crate::connection::ADDED_COLUMNS {
        h.update(format!("added_column={table}.{column}:{}\n", ddl.len()).as_bytes());
        h.update(ddl.as_bytes());
    }
    for (path, bytes) in inputs() {
        h.update(format!("input={path}:{}\n", bytes.len()).as_bytes());
        h.update(bytes.as_bytes());
    }
    format!("{:x}", h.finalize())
}

/// Record that this build's convergence has been applied. Called last by
/// [`crate::open_shared`] and by nothing else.
pub(crate) fn record(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO catalog_convergence (id, fingerprint)
         VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET fingerprint = excluded.fingerprint",
        rusqlite::params![fingerprint()],
    )?;
    Ok(())
}

/// Does this catalog already carry this build's convergence?
///
/// **Two questions, because `converge` persists two things.** The seeds and
/// the schema are what the fingerprint stands for; the `PRAGMA user_version`
/// stamp is separate state in the same file, and a fingerprint that matched
/// while the stamp did not would report a catalog converged that this build
/// has never stamped. That is not hypothetical — every catalog in existence
/// was `user_version` 0 before pd-ja38, and `tests/schema-version/run.sh` §1
/// exists precisely to boot one and require it to come out stamped. It caught
/// this, on a fingerprint-only check that looked obviously sufficient.
///
/// Deliberately a `bool` and not a `Result`: there is exactly one answer this
/// can give that licenses skipping work, and every other outcome — no table,
/// no row, a different hash, an unstamped file, an unreadable one — is the
/// same "no". See the module docs for why that direction is the safe one.
pub(crate) fn is_converged(conn: &Connection) -> bool {
    let stamped = crate::schema_version::version(conn)
        .map(|found| found == crate::schema_version::Database::Shared.version())
        .unwrap_or(false);
    if !stamped {
        return false;
    }
    let want = fingerprint();
    conn.query_row(
        "SELECT fingerprint FROM catalog_convergence WHERE id = 1",
        [],
        |r| r.get::<_, String>(0),
    )
    .map(|found| found == want)
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The drift gate. Every `include_str!` in this crate names a file that
    /// is either hashed into the fingerprint or explicitly classified as not
    /// belonging to the catalog's convergence — and nothing is in both.
    ///
    /// This is the assertion that has to hold for the fingerprint to mean
    /// anything. A seed added to `open_shared` and not to `inputs()` is
    /// silent: the hash goes on matching, a running server goes on skipping
    /// the read-write open, and the new seed never reaches it.
    #[test]
    fn every_catalog_seed_is_in_the_fingerprint() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&src).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for line in text.lines() {
                // Code, never prose. These files name the macro in comments —
                // this module's own scanner is described in one — and a
                // comment that mentions it is not an input.
                if line.trim_start().starts_with("//") {
                    continue;
                }
                let Some((_, rest)) = line.split_once("include_str!(\"") else {
                    continue;
                };
                let Some((raw, _)) = rest.split_once('"') else {
                    continue;
                };
                // `include_str!` is relative to the file; the lists above are
                // relative to the repo. Both spellings reach the same file.
                let repo_relative = match raw.strip_prefix("../../../") {
                    Some(p) => p.to_string(),
                    None => format!("crates/pkdump-db/src/{raw}"),
                };
                found.push(repo_relative);
            }
        }
        assert!(
            found.len() >= inputs().len(),
            "the scan found {} include_str! inputs but the fingerprint names {} — the scan is \
             broken, not the code",
            found.len(),
            inputs().len(),
        );
        let hashed: Vec<&str> = inputs().into_iter().map(|(p, _)| p).collect();
        let excluded: Vec<&str> = NOT_CATALOG_INPUTS.iter().map(|(p, _)| *p).collect();
        for path in &found {
            let in_hash = hashed.contains(&path.as_str());
            let in_excl = excluded.contains(&path.as_str());
            assert!(
                in_hash || in_excl,
                "{path} is embedded in pkdump-db and is classified neither as a catalog \
                 convergence input (convergence::inputs) nor as one that is deliberately not \
                 (convergence::NOT_CATALOG_INPUTS). Decide which it is: a seed the catalog \
                 convergence writes and this list does not hash is invisible — the fingerprint \
                 goes on matching and a running server never applies it.",
            );
            assert!(!(in_hash && in_excl), "{path} is in BOTH convergence lists",);
        }
        for path in hashed {
            assert!(
                found.contains(&path.to_string()),
                "convergence::inputs names {path}, which no include_str! in pkdump-db embeds — \
                 the fingerprint is hashing something that is no longer an input",
            );
        }
    }

    #[test]
    fn the_fingerprint_is_stable_across_calls() {
        assert_eq!(fingerprint(), fingerprint());
        assert_eq!(fingerprint().len(), 64);
    }

    /// Length prefixing, stated as a property rather than as a comment: two
    /// inputs whose bytes run together must not hash to what one longer input
    /// would. Asserted over the real digest so it survives a rewrite of the
    /// hasher.
    #[test]
    fn concatenating_two_inputs_is_not_the_same_as_one() {
        let joined = {
            let mut h = Sha256::new();
            h.update(format!("input=a:{}\n", 2).as_bytes());
            h.update(b"xy");
            h.update(format!("input=b:{}\n", 2).as_bytes());
            h.update(b"zw");
            format!("{:x}", h.finalize())
        };
        let one = {
            let mut h = Sha256::new();
            h.update(format!("input=a:{}\n", 4).as_bytes());
            h.update(b"xyzw");
            format!("{:x}", h.finalize())
        };
        assert_ne!(joined, one);
    }
}
