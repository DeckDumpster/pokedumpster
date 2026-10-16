//! The tenancy kernel has exactly one implementation, and it is dumpster-kit's (sp-rstvfe).
//!
//! PokeDumpster's paths, registry, tenant-connection opener, Access verification and tenant
//! resolution were extracted into `dumpster-kit-tenancy` so a second site would not carry its own
//! copy. The extraction is only worth anything while it stays one: a copy re-added here is
//! a second place to fix a hole, and the two stop agreeing silently. So the rule is stated over
//! the tree rather than over the files that were deleted — the next copy is one nobody has
//! written yet.
//!
//! Each marker below is something only a kernel implementation contains: the registry schema
//! file, the isolation check's query, the JWT validator, the handle character set, the ULID
//! alphabet. `pkdump-db` may BIND the kernel (its prefix, its versions, its collection); it may
//! not reimplement it.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/pkdump-db is two levels below the workspace root")
        .to_path_buf()
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// `(what it is, a string only an implementation of it contains)`.
const MARKERS: [(&str, &str); 6] = [
    (
        "the isolation check",
        "FROM pragma_database_list WHERE name <> 'temp'",
    ),
    ("the JWT validator", "Validation::new"),
    ("the Access cookie name", "CF_Authorization"),
    ("the ULID alphabet", "const CROCKFORD"),
    ("the registry schema", "user_identity_sync_state"),
    ("the registry handle check", "length(CAST(handle AS BLOB))"),
];

#[test]
fn no_crate_carries_its_own_copy_of_the_kernel() {
    let mut all = Vec::new();
    files(&workspace_root().join("crates"), &mut all);
    let mut copies = Vec::new();
    for path in all {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "schema_registry.sql" {
            copies.push(format!("{}: a copy of the registry schema", path.display()));
            continue;
        }
        if !matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("rs" | "sql")
        ) {
            continue;
        }
        if path.ends_with("pkdump-db/tests/kernel_has_one_implementation.rs") {
            continue; // names the markers without being any of them
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (what, marker) in MARKERS {
            if text.contains(marker) {
                copies.push(format!("{}: {what} (`{marker}`)", path.display()));
            }
        }
    }
    assert!(
        copies.is_empty(),
        "the tenancy kernel is dumpster-kit-tenancy's alone; fix it there and re-pin, do not \
         copy it back:\n  {}",
        copies.join("\n  ")
    );
}

/// The vacuity guard: a scan that finds nothing because it scanned nothing proves nothing. The
/// markers must be real, so the kernel's own crate — checked out by cargo — must contain every
/// one of them. (If the kit renames a thing, this fails and says which marker went stale.)
#[test]
fn every_marker_is_something_the_kernel_really_contains() {
    // Not `cargo metadata`: offline it must resolve every platform's dependencies, which a
    // gate's cargo cache does not hold, and it took minutes even where it worked. Not a path
    // under `$CARGO_HOME` either: a canary builds against a kit that is a path or patched
    // dependency and has no git checkout at all. The one place that is true wherever cargo found
    // the kit is the dep-info it wrote when it compiled the crate, next to this test binary.
    let deps = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let kit_src = std::fs::read_dir(&deps)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().to_string();
            n.starts_with("dumpster_kit_tenancy-") && n.ends_with(".d")
        })
        // A target dir keeps the dep-info of every revision it has built; the live one is newest.
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .and_then(|p| {
            let text = std::fs::read_to_string(p).ok()?;
            let lib = text
                .split_once(": ")?
                .1
                .split_whitespace()
                .next()?
                .to_string();
            Path::new(&lib).parent().map(Path::to_path_buf)
        })
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| {
            panic!(
                "no dep-info for dumpster-kit-tenancy under {}",
                deps.display()
            )
        });
    let mut all = Vec::new();
    files(&kit_src, &mut all);
    let kernel: String = all
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    for (what, marker) in MARKERS {
        assert!(
            kernel.contains(marker),
            "marker for {what} (`{marker}`) is not in the kernel any more — update the marker"
        );
    }
}
