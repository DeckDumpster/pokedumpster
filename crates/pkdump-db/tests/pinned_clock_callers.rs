//! `pkdump_db::clock::pin` replaces the clock, and exactly one thing in this
//! workspace is allowed to: the fixture seeder.
//!
//! It is a public function on a public crate because `pkdump seed-fixture`
//! lives in another crate and seeds through the repository functions
//! deliberately — there is no visibility that expresses "the fixture, and
//! nothing else". So the rule is stated here instead, over the tree, the way
//! `pkdump-server`'s `no_catalog_writer.rs` states its own.
//!
//! A pin that reached production would stamp rows with an instant that is not
//! the time, and every one of them would look completely ordinary.

use std::path::{Path, PathBuf};

/// The one file allowed to pin, relative to the workspace root.
const ALLOWED: &str = "crates/pkdump-cli/src/fixture.rs";

/// Files that NAME the call without making it: the module that defines it,
/// and this one, which cannot state the rule without spelling it. Everything
/// else in the workspace is scanned.
const EXEMPT: [&str; 2] = [
    "crates/pkdump-db/src/clock.rs",
    "crates/pkdump-db/tests/pinned_clock_callers.rs",
];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/pkdump-db is two levels below the workspace root")
        .to_path_buf()
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn only_the_fixture_seeder_pins_the_clock() {
    let root = workspace_root();
    let mut sources = Vec::new();
    rust_sources(&root.join("crates"), &mut sources);
    // Not vacuous: a walk that found nothing would pass whatever the
    // workspace does.
    assert!(
        sources.len() > 50,
        "walked only {} rust files under {root:?}/crates",
        sources.len()
    );

    let mut callers = Vec::new();
    for path in &sources {
        let rel = path.strip_prefix(&root).unwrap().to_string_lossy().to_string();
        if rel == ALLOWED || EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let body = std::fs::read_to_string(path).unwrap();
        for (i, line) in body.lines().enumerate() {
            if line.contains("clock::pin(") || line.contains("clock::unpin(") {
                callers.push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }

    assert!(
        callers.is_empty(),
        "the clock may only be pinned by {ALLOWED}, which builds a fixture \
         that has to be reproducible. Anything else stamps rows with an \
         instant that is not the time:\n  {}",
        callers.join("\n  "),
    );

    // And the allowed caller must still BE one — a guard that only forbids
    // passes just as happily on a seeder that has stopped pinning anything.
    let fixture = std::fs::read_to_string(root.join(ALLOWED)).unwrap();
    assert!(
        fixture.contains("clock::pin("),
        "{ALLOWED} no longer pins the clock, so the committed fixture is \
         back to recording the minute it was built"
    );
}
