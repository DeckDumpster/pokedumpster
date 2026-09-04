//! Nothing on the serving path opens the shared catalog read-write (pd-dzu5).
//!
//! `pkdump serve` must *have* the catalog converged before it answers a
//! request, and for that one purpose it calls
//! `pkdump_db::open_shared_for_serving`, which asks read-only first and takes
//! the write lock only when this build genuinely has something to apply. That
//! is the whole of the fix, and it is one call site — so the way it comes
//! undone is somebody adding a second one, in a route or a helper, where the
//! cost is invisible: an ordinary restart starts competing again with the
//! nightly `pkdump-lake-derive shared`, and the box is only down on the nights
//! a deploy happens to land inside the build.
//!
//! Stated over the crate's source rather than over the two files that are
//! right today, for the same reason `crates/pkdump-keys/tests/separation.rs`
//! is: it has to stay true of code nobody has written yet.

/// Every `.rs` under `crates/pkdump-server/src`, with each file truncated at
/// its first `#[cfg(test)]` — test code legitimately builds catalogs to serve
/// from, and a fixture is not the serving path.
fn serving_source() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let production = match text.find("#[cfg(test)]") {
                Some(i) => text[..i].to_string(),
                None => text,
            };
            out.push((path.display().to_string(), production));
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&src, &mut out);
    assert!(
        !out.is_empty(),
        "found no source to check — the walk is broken"
    );
    out
}

#[test]
fn the_server_never_opens_the_catalog_read_write() {
    for (file, text) in serving_source() {
        for (n, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for call in ["open_shared(", "open_shared_with_patience("] {
                assert!(
                    !line.contains(call),
                    "{file}:{} calls {call}…), which opens the shared catalog READ-WRITE.\n\
                     The serving path may not: every start would then take the catalog's write \
                     lock and race the nightly `pkdump-lake-derive shared`, which holds it for \
                     minutes (pd-dzu5). Use pkdump_db::open_shared_for_serving, which asks \
                     read-only first and writes only when this build really has something to \
                     converge.",
                    n + 1,
                );
            }
        }
    }
}

/// And the positive half: the one call that is allowed is still there. A guard
/// that only forbids passes just as happily on a server that opens no catalog
/// at all — which would be a server whose data-only migrations never apply.
#[test]
fn the_server_still_converges_the_catalog_before_it_serves() {
    let found = serving_source()
        .into_iter()
        .any(|(_, text)| text.contains("open_shared_for_serving("));
    assert!(
        found,
        "no call to open_shared_for_serving anywhere in pkdump-server. The startup convergence \
         is what applies a data-only migration a binary upgrade ships; without it the server \
         serves whatever shape the catalog happens to be in."
    );
}
