//! The catalog refresh writes nothing it does not own (pd-hkbc, pd-lunn).
//!
//! `pkdump data refresh` LANDS. It writes not one byte of any tenant database,
//! and since pd-lunn not one byte of `shared.sqlite` either. That is a claim
//! about a whole binary, so it is checked by running the real binary over a
//! data directory that HAS tenants in it and seeing whether anything moved.
//!
//! The upstream is the in-process `pkdump_ingest::test_upstream` server (this
//! replaces `tests/refresh/upstream.py`, sp-ohyp5m). It publishes nothing: no
//! sets, no groups. Every phase still runs and the run reaches its end, which
//! is the only part these tests are about. A refresh that died at its first
//! fetch would report clean tenant bytes and prove nothing, so each test
//! asserts the run completed and that bytes really reached the landing zone.
//!
//! The second test is pd-nons / pd-llbq: a dead pokemontcg.io no longer ends
//! the run. The shipped binary retries on the budget its environment names,
//! exits 2, and still lands TCGCSV — and the partition it leaves says which
//! half is short.

use std::collections::BTreeMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Output;

use pkdump_ingest::test_upstream::{FakeUpstream, Reply};
use pkdump_ingest::upstream::{
    ENV_POKEMON_TCG_DATA_BASE_URL, ENV_POKEMONTCG_BASE_URL, ENV_TCGCSV_BASE_URL,
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/ui/fixtures")
}

/// Every route the refresh asks for, answered with a well-formed EMPTY result;
/// `/v2-down` is the day api.pokemontcg.io had on 2026-08-11. Anything else is
/// a 404 naming the path — the fixture's assumptions drifting must read as a
/// failure, not as a quiet fallback.
fn publishes_nothing(target: &str, _n: usize) -> Reply {
    let path = target.split('?').next().unwrap_or(target);
    let is_groups = path
        .strip_prefix("/tcgplayer/")
        .and_then(|r| r.strip_suffix("/groups"))
        .is_some_and(|c| !c.is_empty() && c.bytes().all(|b| b.is_ascii_digit()));
    if is_groups {
        Reply::ok(r#"{"results":[],"success":true,"errors":[]}"#)
    } else if path == "/v2/sets" {
        Reply::ok(r#"{"data":[],"page":1,"pageSize":250,"count":0,"totalCount":0}"#)
    } else if path == "/v2-down/sets" {
        Reply::status(502, r#"{"error":"upstream is having 2026-08-11"}"#)
    } else if path == "/PokemonTCG/pokemon-tcg-data/tar.gz/refs/heads/master" {
        // land_bulk lands these bytes without unpacking; any payload lands.
        Reply::ok("{}")
    } else {
        Reply::status(404, format!(r#"{{"error":"no route for {path}"}}"#))
    }
}

struct Rig {
    _root: tempfile::TempDir,
    data: PathBuf,
    lake: PathBuf,
    upstream: FakeUpstream,
}

impl Rig {
    /// A data directory with a converged catalog and two provisioned tenants,
    /// each holding a real collection. An empty collection would let "the
    /// refresh wrote nothing" pass for the wrong reason.
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let lake = root.path().join("lake");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&lake).unwrap();
        std::fs::copy(fixtures().join("shared.sqlite"), data.join("shared.sqlite")).unwrap();
        let rig = Self {
            _root: root,
            data,
            lake,
            upstream: FakeUpstream::start(publishes_nothing),
        };

        // The refresh opens the catalog READ-ONLY and so cannot heal it; this
        // is the cheapest command that opens it read-write (schema + seeds,
        // no row written). The scaffolding must not depend on the thing under
        // test converging anything.
        let out = rig.pkdump(
            &["data", "apply-corrections", "--db"],
            &[
                rig.data.join("shared.sqlite").to_str().unwrap(),
                "--dry-run",
            ],
            &[],
        );
        assert!(out.status.success(), "converge: {}", text(&out.stderr));

        // Provisioned by the REAL `tenant create`, so the registry and the
        // opaque-id layout are prod's. `collection` is the handle
        // $PKDUMP_USER defaults to — the database the deleted step 7 wrote.
        for handle in ["collection", "alice"] {
            let out = rig.pkdump(&["tenant", "create", handle], &[], &[]);
            assert!(out.status.success(), "tenant create: {}", text(&out.stderr));
        }
        let dbs = rig.tenant_dbs();
        assert_eq!(dbs.len(), 2, "two tenants provisioned");
        for db in &dbs {
            std::fs::copy(fixtures().join("collection.sqlite"), db).unwrap();
            // Sidecars belong to the database just overwritten.
            for ext in ["sqlite-wal", "sqlite-shm"] {
                let _ = std::fs::remove_file(db.with_extension(ext));
            }
            let conn = rusqlite::Connection::open_with_flags(
                db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let rows: i64 = conn
                .query_row("SELECT count(*) FROM collection", [], |r| r.get(0))
                .unwrap();
            assert!(
                rows >= 2,
                "the fixture collection has {rows} rows; with nothing in it the deleted \
                 step 7 would have written nothing either and this gate would pass on a \
                 binary that still has it"
            );
        }
        rig
    }

    fn tenant_dbs(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(self.data.join("tenants"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "sqlite"))
            .collect();
        v.sort();
        v
    }

    fn pkdump(&self, args: &[&str], more: &[&str], env: &[(&str, String)]) -> Output {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_pkdump"));
        cmd.args(args)
            .args(more)
            .env("PKDUMP_HOME", &self.data)
            .env("PKDUMP_USER", "collection")
            .env("PKDUMP_LAKE_DIR", &self.lake)
            .env("PKDUMP_LAKE_ENV", self.data.join("no-such-lake.env"))
            .env(
                ENV_TCGCSV_BASE_URL,
                format!("{}/tcgplayer", self.upstream.base_url()),
            )
            .env(
                ENV_POKEMONTCG_BASE_URL,
                format!("{}/v2", self.upstream.base_url()),
            )
            .env(ENV_POKEMON_TCG_DATA_BASE_URL, self.upstream.base_url());
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.output().expect("the pkdump binary did not run")
    }

    fn refresh(&self) -> Output {
        self.pkdump(&["data", "refresh"], &[], &[])
    }

    /// Every file under `tenants/`, by path. WAL and shared-memory sidecars are
    /// included on purpose: merely OPENING a tenant database in WAL mode
    /// creates a `-wal` beside it, and that is a tenant byte too.
    fn tenant_fingerprint(&self) -> BTreeMap<String, u64> {
        fingerprint(&self.data, |rel| rel.starts_with("tenants/"))
    }

    /// Everything in the data directory, so a stray write somewhere new — a
    /// registry row, a fresh tenant file — cannot hide. The catalog's own
    /// `-wal`/`-shm` are excluded: opening a WAL database read-only still
    /// creates the wal-index and cannot clean it up on close. What matters is
    /// that the catalog's CONTENT did not move, and it is hashed itself.
    fn data_fingerprint(&self) -> BTreeMap<String, u64> {
        fingerprint(&self.data, |rel| {
            rel != "shared.sqlite-wal" && rel != "shared.sqlite-shm"
        })
    }

    fn landed_parts(&self) -> usize {
        walk(&self.lake)
            .iter()
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("part-"))
            })
            .count()
    }

    fn manifest_of(&self, dataset: &str) -> Option<String> {
        let needle = format!("dataset={dataset}");
        walk(&self.lake)
            .into_iter()
            .find(|p| {
                p.file_name().is_some_and(|n| n == "_manifest.json")
                    && p.components()
                        .any(|c| c.as_os_str().to_string_lossy() == needle)
            })
            .map(|p| std::fs::read_to_string(p).unwrap())
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn fingerprint(root: &Path, keep: impl Fn(&str) -> bool) -> BTreeMap<String, u64> {
    walk(root)
        .into_iter()
        .filter_map(|p| {
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
            keep(&rel).then(|| {
                let bytes = std::fs::read(&p).unwrap();
                let mut h = DefaultHasher::new();
                bytes.hash(&mut h);
                (rel, h.finish())
            })
        })
        .collect()
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn a_refresh_lands_and_writes_not_one_byte_of_any_tenant_or_the_catalog() {
    let rig = Rig::new();
    let tenants = rig.tenant_fingerprint();
    let inventory = rig.data_fingerprint();
    let shared = inventory["shared.sqlite"];
    assert!(!tenants.is_empty());

    let out = rig.refresh();
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    assert!(
        out.status.success(),
        "the refresh must complete — a run that died early proves nothing:\n{stdout}\n{stderr}\n\
         asked: {:?}",
        rig.upstream.requests()
    );
    assert_eq!(
        count(&stdout, "Refresh complete: landed, not derived"),
        1,
        "{stdout}"
    );

    // It really did the work rather than skipping to the end.
    for phase in [
        "Landing raw upstream responses in ",
        "Landing the newest sets from pokemontcg.io",
        "Landing TCGCSV groups, products, prices",
        "Landing the Pokémon Japan catalog",
    ] {
        assert!(
            stdout.contains(phase),
            "phase did not run: {phase}\n{stdout}"
        );
    }
    assert!(
        stdout.contains(&format!(
            "Opening shared catalog READ-ONLY at {}",
            rig.data.join("shared.sqlite").display()
        )),
        "the catalog was not opened read-only:\n{stdout}"
    );
    // And the bytes left the process: a run that fetched and dropped them
    // would satisfy every other assertion here.
    assert!(rig.landed_parts() > 0, "nothing was landed");
    assert!(
        walk(&rig.lake).iter().any(|p| {
            p.file_name().is_some_and(|n| n == "_manifest.json")
                && std::fs::read_to_string(p)
                    .unwrap()
                    .contains("\"complete\": true")
        }),
        "no manifest says the run completed"
    );

    // pd-lunn: the catalog did not move, and nothing was derived into it.
    let after = rig.data_fingerprint();
    assert_eq!(after["shared.sqlite"], shared, "shared.sqlite changed");
    let conn = rusqlite::Connection::open_with_flags(
        rig.data.join("shared.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let derived: i64 = conn
        .query_row("SELECT COUNT(*) FROM raw_derivation", [], |r| r.get(0))
        .unwrap();
    assert_eq!(derived, 0, "something was derived into the catalog");

    // pd-hkbc / pd-s5yn: not one byte of any tenant database.
    assert_eq!(rig.tenant_fingerprint(), tenants, "a tenant database moved");
    assert_eq!(
        after, inventory,
        "the data directory gained or changed files"
    );

    // Once could be an accident of ordering. A second run lands into the same
    // ingest_date under a new run ULID — what a retry does in production.
    let second = rig.refresh();
    assert!(second.status.success(), "{}", text(&second.stderr));
    assert_eq!(rig.data_fingerprint(), inventory, "a second refresh wrote");
}

#[test]
fn a_dead_pokemontcg_io_exits_2_lands_tcgcsv_and_says_which_half_is_short() {
    let rig = Rig::new();
    let tenants = rig.tenant_fingerprint();
    let inventory = rig.data_fingerprint();

    let out = rig.pkdump(
        &["data", "refresh"],
        &[],
        &[
            (
                ENV_POKEMONTCG_BASE_URL,
                format!("{}/v2-down", rig.upstream.base_url()),
            ),
            ("PKDUMP_HTTP_RETRY_ATTEMPTS", "3".into()),
            ("PKDUMP_HTTP_RETRY_BASE_MS", "25".into()),
        ],
    );
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));

    // 2, not 1 and not 0: 1 is "the run failed", 0 would say a stale set list
    // is nothing to mention.
    assert_eq!(out.status.code(), Some(2), "{stdout}\n{stderr}");
    assert_eq!(count(&stderr, "Refresh PARTIAL"), 1, "{stderr}");

    // It retried on the budget the environment named: 3 attempts, not 1.
    let hits = rig
        .upstream
        .requests()
        .iter()
        .filter(|r| r.starts_with("/v2-down/sets"))
        .count();
    assert_eq!(hits, 3, "the tail did not spend its whole retry budget");

    // It carried on past the failure, and AFTER it: the order is the
    // difference between a partial night and a lost one.
    let tail = stdout.find("Landing the newest sets from pokemontcg.io");
    let tcgcsv = stdout.find("Landing TCGCSV groups, products, prices");
    assert!(
        matches!((tail, tcgcsv), (Some(t), Some(c)) if t < c),
        "TCGCSV was not landed after the tail gave up:\n{stdout}"
    );
    assert!(rig.landed_parts() > 0, "the partial run left no bytes");

    // The partition says which half is short. `complete` is per dataset, so a
    // night only the tail lost is short in the tail and whole in TCGCSV — the
    // input the offline derive reads to tell a partial night (2) from an
    // underivable one (1).
    let sets = rig
        .manifest_of("sets")
        .expect("the dead tail landed no manifest");
    assert_eq!(count(&sets, "\"complete\": false"), 1, "{sets}");
    assert_eq!(count(&sets, "\"status\": 502"), 1, "{sets}");
    // ONE failure, not one per attempt: a failure means "this URL was not
    // fetched", and a recovered hiccup must not mark the night incomplete.
    assert_eq!(count(&sets, "\"url\":"), 1, "{sets}");
    let groups = rig
        .manifest_of("groups")
        .expect("TCGCSV landed no manifest");
    assert_eq!(count(&groups, "\"complete\": true"), 1, "{groups}");

    let runs: std::collections::BTreeSet<String> = walk(&rig.lake)
        .iter()
        .flat_map(|p| p.components().collect::<Vec<_>>())
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .filter(|c| c.starts_with("run="))
        .collect();
    assert_eq!(runs.len(), 1, "one run landed this night: {runs:?}");

    // A partial run still writes no tenant database and no catalog byte.
    assert_eq!(rig.tenant_fingerprint(), tenants, "a tenant database moved");
    assert_eq!(
        rig.data_fingerprint(),
        inventory,
        "the data directory changed"
    );
}
