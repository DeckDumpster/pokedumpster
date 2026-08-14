//! The shipper, end to end over a real data directory and a real object
//! store — the four claims `pd-dxn3` asks to see proven, plus the two the
//! design rests on.
//!
//! | claim                         | test                                          |
//! |-------------------------------|-----------------------------------------------|
//! | gap detection                 | `a_dropped_sequence_number_is_detected…`      |
//! | idempotence                   | `shipping_the_same_rows_twice_leaves…`        |
//! | resumability                  | `a_crash_after_a_part_lands_but_before…`      |
//! | encrypted, under the RIGHT key| `each_tenant_s_parts_open_only_under…`        |
//! | a tombstone stops shipping    | `a_tombstoned_tenant_is_not_shipped…`         |
//! | absence is not permission     | `an_unregistered_tenant_is_skipped…`          |
//! | a backfill is an ordinary run | `a_backfilled_collection_ships_as_ordinary…`  |
//!
//! The object store is a [`DirStore`], which is the same `ObjectStore` the
//! real thing is behind and holds exactly the keys and bytes S3 would — so
//! "the zone is unchanged" can be a byte-for-byte comparison of what is on
//! disk rather than a claim about an API. `tests/lake/shipper.sh` is the
//! other half: the same binary against a real MinIO, under the real tenant
//! credential policy, with a real `SIGKILL`.
//!
//! Every fixture here is treated as if it were real tenant data — which is to
//! say there is none of it. The collections hold invented printing ids.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use pkdump_lake::{DirStore, ObjectStore, TenantZoneConfig};
use pkdump_ship::run::{Outcome, Status};
use pkdump_ship::{cipher, cursor, encode};
use rusqlite::Connection;

const ALICE: &str = "01J0000000000000000000000A";
const BOB: &str = "01J0000000000000000000000B";

// ── the world one test runs in ──────────────────────────────────────────────

/// The environment this crate's jobs read is process-wide, so tests that set
/// it cannot overlap. Same shape as `pkdump-keys`' own guard.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct World {
    _dir: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
    home: std::path::PathBuf,
    zone_root: std::path::PathBuf,
    previous_home: Option<String>,
    previous_key: Option<String>,
}

impl World {
    /// A data directory with a registry, a master key, and an empty zone.
    fn new() -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("data");
        let zone_root = dir.path().join("zone");
        std::fs::create_dir_all(home.join("tenants")).unwrap();
        std::fs::create_dir_all(&zone_root).unwrap();

        let previous_home = std::env::var("PKDUMP_HOME").ok();
        let previous_key = std::env::var(pkdump_keys::master::KEY_ENV_FILE).ok();
        unsafe {
            std::env::set_var("PKDUMP_HOME", &home);
            std::env::set_var(
                pkdump_keys::master::KEY_ENV_FILE,
                dir.path().join("tenant-master.key"),
            );
        }
        pkdump_keys::master::create().unwrap();

        World {
            _dir: dir,
            _lock: lock,
            home,
            zone_root,
            previous_home,
            previous_key,
        }
    }

    /// Register `handle` with a chosen `database_id`, create their collection
    /// database, and register their key.
    fn tenant(&self, handle: &str, database_id: &str) -> pkdump_db::tenants::Tenant {
        let registry = pkdump_db::registry::open().unwrap();
        registry
            .execute(
                "INSERT INTO user (database_id, handle, created_at, state) \
                 VALUES (?1, ?2, '2026-08-14T00:00:00Z', 'active')",
                rusqlite::params![database_id, handle],
            )
            .unwrap();
        pkdump_keys::state::register(&registry, database_id).unwrap();
        let path = self
            .home
            .join("tenants")
            .join(format!("{database_id}.sqlite"));
        pkdump_db::open_user(&path).unwrap();
        pkdump_db::tenants::list()
            .unwrap()
            .into_iter()
            .find(|t| t.user.database_id == database_id)
            .expect("just registered")
    }

    fn collection(&self, database_id: &str) -> Connection {
        pkdump_db::open_user(
            &self
                .home
                .join("tenants")
                .join(format!("{database_id}.sqlite")),
        )
        .unwrap()
    }

    fn registry(&self) -> Connection {
        pkdump_db::registry::open().unwrap()
    }

    fn zone(&self) -> DirStore {
        DirStore::new(&self.zone_root)
    }

    /// Every object in the zone, key -> bytes. The unit "the zone is
    /// unchanged" is measured in.
    fn objects(&self) -> BTreeMap<String, Vec<u8>> {
        let mut out = BTreeMap::new();
        walk(&self.zone_root, &self.zone_root, &mut out);
        out
    }
}

impl Drop for World {
    fn drop(&mut self) {
        unsafe {
            match &self.previous_home {
                Some(v) => std::env::set_var("PKDUMP_HOME", v),
                None => std::env::remove_var("PKDUMP_HOME"),
            }
            match &self.previous_key {
                Some(v) => std::env::set_var(pkdump_keys::master::KEY_ENV_FILE, v),
                None => std::env::remove_var(pkdump_keys::master::KEY_ENV_FILE),
            }
        }
    }
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out);
        } else {
            let key = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            out.insert(key, std::fs::read(&path).unwrap());
        }
    }
}

fn config() -> TenantZoneConfig {
    TenantZoneConfig::from_settings(
        &[(
            pkdump_lake::tenant::KEY_TENANT_PROFILE.to_string(),
            "pkdump-tenant-test".to_string(),
        )]
        .into_iter()
        .collect(),
        None,
    )
    .unwrap()
}

/// Append `n` holdings to a collection, dated `date`. The triggers write the
/// outbox; nothing here mentions it, which is the point of pd-5m54.
fn add_holdings(conn: &Connection, n: usize, date: &str) {
    let before = high_water(conn);
    for i in 0..n {
        conn.execute(
            "INSERT INTO collection (printing_id, acquired_at, source) VALUES (?1, ?2, 'test')",
            rusqlite::params![format!("fixture-{date}-{i}"), format!("{date}T00:00:00Z")],
        )
        .unwrap();
    }
    redate(conn, before, date);
}

fn high_water(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT coalesce(max(seq), 0) FROM ownership_outbox",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

/// Move every event after `after` onto `date`, keeping its time of day.
///
/// The trigger reads the clock, and these tests are about partitioning by
/// date — so the fixture says which day each batch of events belongs to,
/// exactly as a run on that day would have found it. Bounded by `after` so a
/// later batch cannot re-date an earlier one.
fn redate(conn: &Connection, after: i64, date: &str) {
    conn.execute(
        "UPDATE ownership_outbox SET occurred_at = ?1 || substr(occurred_at, 11) \
         WHERE seq > ?2",
        rusqlite::params![date, after],
    )
    .unwrap();
}

fn ship(
    world: &World,
    tenants: &[pkdump_db::tenants::Tenant],
    max_rows: usize,
) -> pkdump_ship::Report {
    pkdump_ship::ship_all(
        &world.zone(),
        &config(),
        &world.registry(),
        tenants,
        max_rows,
    )
}

// ── the run ─────────────────────────────────────────────────────────────────

#[test]
fn a_first_run_ships_every_event_under_the_tenant_s_own_prefix() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 5, "2026-08-14");

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(report.events(), 5);
    assert_eq!(report.parts(), 1);

    let objects = world.objects();
    assert_eq!(objects.len(), 1);
    let key = objects.keys().next().unwrap();
    assert_eq!(
        key,
        &format!(
            "tenant/database_id={ALICE}/dataset=holdings/as_of=2026-08-14/\
             part-seq-000000000001-000000000005.parquet.enc"
        ),
        "the key names the range it carries, under this tenant's prefix"
    );

    // …and the cursor now says so.
    assert_eq!(cursor::shipped_thru(&world.collection(ALICE)).unwrap(), 5);
    assert_eq!(cursor::pending(&world.collection(ALICE)).unwrap(), 0);
}

#[test]
fn a_second_run_ships_only_what_arrived_since() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 3, "2026-08-14");
    ship(&world, std::slice::from_ref(&alice), 100);

    add_holdings(&world.collection(ALICE), 2, "2026-08-15");
    let report = ship(&world, &[alice], 100);

    assert_eq!(report.events(), 2, "only the new rows");
    let keys: Vec<_> = world.objects().into_keys().collect();
    assert_eq!(keys.len(), 2);
    assert!(keys[1].contains("as_of=2026-08-15"));
    assert!(keys[1].ends_with("part-seq-000000000004-000000000005.parquet.enc"));
}

#[test]
fn a_part_never_spans_two_days_and_a_long_run_is_split() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-08-13");
    add_holdings(&conn, 5, "2026-08-14");

    ship(&world, &[alice], 3);
    let keys: Vec<_> = world.objects().into_keys().collect();
    assert_eq!(
        keys.iter()
            .map(|k| k.rsplit('/').next().unwrap())
            .collect::<Vec<_>>(),
        [
            "part-seq-000000000001-000000000003.parquet.enc",
            "part-seq-000000000004-000000000006.parquet.enc",
            "part-seq-000000000007-000000000008.parquet.enc",
        ]
    );
    assert!(keys[0].contains("as_of=2026-08-13"));
    assert!(keys[1].contains("as_of=2026-08-14"));
}

// ── idempotence ─────────────────────────────────────────────────────────────

/// The bead's claim, in its own words: ship the same outbox range twice, the
/// tenant zone is UNCHANGED by the second run.
#[test]
fn shipping_the_same_rows_twice_leaves_the_zone_byte_identical() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 9, "2026-08-14");

    ship(&world, std::slice::from_ref(&alice), 4);
    let after_first = world.objects();
    assert_eq!(after_first.len(), 3);

    // Wind the cursor back, which is what a redrive of the same range is.
    world
        .collection(ALICE)
        .execute("DELETE FROM ownership_outbox_cursor", [])
        .unwrap();

    let report = ship(&world, &[alice], 4);
    assert_eq!(report.events(), 9, "it really did ship them all again");
    assert_eq!(
        world.objects(),
        after_first,
        "the second run wrote different bytes, or extra objects"
    );
}

// ── resumability ────────────────────────────────────────────────────────────

/// A store that writes the object and then dies, which is the one instant
/// at-least-once delivery is about: the part is in the zone and the cursor
/// does not know it. The container gate does this with a real `SIGKILL` on a
/// real process; here it is a panic, so it lands on the same part every time.
struct DiesAfter {
    inner: DirStore,
    after: usize,
    puts: std::sync::atomic::AtomicUsize,
}

impl ObjectStore for DiesAfter {
    fn put(&self, key: &str, body: Vec<u8>) -> pkdump_lake::Result<()> {
        self.inner.put(key, body)?;
        let done = self.puts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        if done >= self.after {
            panic!("the shipper was killed after {done} part(s)");
        }
        Ok(())
    }

    fn describe(&self) -> String {
        self.inner.describe()
    }
}

#[test]
fn a_crash_after_a_part_lands_but_before_the_cursor_moves_re_ships_only_that_part() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 12, "2026-08-14");

    // Killed the instant the second part lands.
    let dying = DiesAfter {
        inner: DirStore::new(&world.zone_root),
        after: 2,
        puts: std::sync::atomic::AtomicUsize::new(0),
    };
    let registry = world.registry();
    let killed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pkdump_ship::ship_one(&dying, &config(), &registry, &alice, 4)
    }));
    assert!(killed.is_err(), "the fixture did not actually kill the run");

    let after_crash = world.objects();
    assert_eq!(after_crash.len(), 2, "two parts landed before the crash");
    assert_eq!(
        cursor::shipped_thru(&world.collection(ALICE)).unwrap(),
        4,
        "…and the cursor only knows about the first — the second is unrecorded, \
         which is exactly the state a re-ship has to handle"
    );

    // Restart.
    let report = ship(&world, &[alice], 4);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(
        report.events(),
        8,
        "the restart re-ships the part the crash did not record, and the rest"
    );

    let objects = world.objects();
    assert_eq!(
        objects.len(),
        3,
        "no duplicate object beside the re-shipped one"
    );
    for (key, bytes) in &after_crash {
        assert_eq!(
            objects.get(key),
            Some(bytes),
            "{key} changed when it was shipped again"
        );
    }
    assert_eq!(cursor::shipped_thru(&world.collection(ALICE)).unwrap(), 12);

    // Nothing was lost: every event is in the zone exactly once.
    let mut seqs: Vec<i64> = Vec::new();
    for (key, bytes) in &objects {
        let tenant_key = pkdump_keys::tenant_key(&world.registry(), ALICE).unwrap();
        let parquet = cipher::open(&tenant_key, key, bytes).unwrap();
        seqs.extend(encode::decode(parquet).unwrap().into_iter().map(|e| e.seq));
    }
    seqs.sort_unstable();
    assert_eq!(seqs, (1..=12).collect::<Vec<_>>());
}

// ── gap detection ───────────────────────────────────────────────────────────

/// Deliberately drop a sequence number in transit, exactly as the bead asks.
/// Deleting an unshipped outbox row is what a lost event looks like from
/// here — the number is gone and, because `AUTOINCREMENT` never reissues one,
/// it is never coming back.
#[test]
fn a_dropped_sequence_number_is_detected_recorded_and_alarmed() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 6, "2026-08-14");
    conn.execute("DELETE FROM ownership_outbox WHERE seq IN (3, 4)", [])
        .unwrap();

    let report = ship(&world, &[alice], 100);

    assert_eq!(
        report.outcome(),
        Outcome::Gap,
        "a lost event must not come out as a clean run"
    );
    assert_eq!(report.outcome().code(), 3);
    assert_eq!(
        report.gaps(),
        [(
            ALICE,
            pkdump_ship::Gap {
                from_seq: 3,
                to_seq: 4
            }
        )]
    );

    // Durable, not just printed: the journal rotates, this does not.
    let recorded = cursor::gaps(&conn).unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!((recorded[0].from_seq, recorded[0].to_seq), (3, 4));
    assert!(!recorded[0].detected_at.is_empty());
}

/// The other half of the decision: a gap is alarmed, not obeyed. The rows
/// that are still there are already lost if the shipper refuses to move.
#[test]
fn a_gap_does_not_stop_the_events_that_are_still_there() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 6, "2026-08-14");
    conn.execute("DELETE FROM ownership_outbox WHERE seq = 3", [])
        .unwrap();

    let report = ship(&world, std::slice::from_ref(&alice), 100);
    assert_eq!(report.events(), 5, "everything that survived was shipped");
    assert_eq!(cursor::shipped_thru(&conn).unwrap(), 6);

    // A second run neither re-detects the gap (the cursor is past it) nor
    // records a second copy of it — the ledger is the durable answer.
    let again = ship(&world, &[alice], 100);
    assert!(again.gaps().is_empty());
    assert_eq!(cursor::gaps(&conn).unwrap().len(), 1);
}

#[test]
fn a_gap_at_the_head_of_the_batch_is_seen_too() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 4, "2026-08-14");
    // The first two events never reach the shipper at all.
    conn.execute("DELETE FROM ownership_outbox WHERE seq <= 2", [])
        .unwrap();

    let report = ship(&world, &[alice], 100);
    assert_eq!(
        report.gaps(),
        [(
            ALICE,
            pkdump_ship::Gap {
                from_seq: 1,
                to_seq: 2
            }
        )],
        "a hole before the first surviving row is the one an implementation forgets"
    );
}

// ── the key, and whose it is ────────────────────────────────────────────────

#[test]
fn each_tenant_s_parts_open_only_under_that_tenant_s_own_key() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    add_holdings(&world.collection(BOB), 2, "2026-08-14");

    let report = ship(&world, &[alice, bob], 100);
    assert_eq!(report.outcome(), Outcome::Clean);

    let objects = world.objects();
    assert_eq!(objects.len(), 2);
    let registry = world.registry();
    let alice_key = pkdump_keys::tenant_key(&registry, ALICE).unwrap();
    let bob_key = pkdump_keys::tenant_key(&registry, BOB).unwrap();

    for (key, bytes) in &objects {
        let (mine, theirs) = if key.contains(ALICE) {
            (&alice_key, &bob_key)
        } else {
            (&bob_key, &alice_key)
        };
        // Not plaintext…
        assert!(
            !bytes.windows(4).any(|w| w == b"PAR1"),
            "{key} looks like a bare Parquet file — it is not encrypted"
        );
        assert!(
            !bytes.windows(7).any(|w| w == b"fixture"),
            "{key} carries a readable printing id"
        );
        // …opens under its own key…
        let parquet = cipher::open(mine, key, bytes).expect("its own key must open it");
        assert_eq!(&parquet[..4], b"PAR1");
        assert_eq!(encode::decode(parquet).unwrap().len(), 2);
        // …and not under the other tenant's.
        assert!(
            cipher::open(theirs, key, bytes).is_err(),
            "{key} opened under the WRONG tenant's key"
        );
    }
}

#[test]
fn what_lands_in_the_zone_is_what_the_outbox_held() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-08-14");
    conn.execute("UPDATE collection SET notes = 'traded' WHERE id = 2", [])
        .unwrap();
    conn.execute("DELETE FROM collection WHERE id = 1", [])
        .unwrap();
    redate(&conn, 3, "2026-08-14");

    ship(&world, &[alice], 100);
    let (key, bytes) = world.objects().into_iter().next().unwrap();
    let tenant_key = pkdump_keys::tenant_key(&world.registry(), ALICE).unwrap();
    let events = encode::decode(cipher::open(&tenant_key, &key, &bytes).unwrap()).unwrap();

    assert_eq!(
        events.iter().map(|e| e.op.as_str()).collect::<Vec<_>>(),
        ["insert", "insert", "insert", "update", "delete"]
    );
    assert!(events.iter().all(|e| e.source_table == "collection"));
    let payload: serde_json::Value = serde_json::from_str(&events[3].payload).unwrap();
    assert_eq!(payload["notes"], "traded");
    let deleted: serde_json::Value = serde_json::from_str(&events[4].payload).unwrap();
    assert_eq!(
        deleted["printing_id"], "fixture-2026-08-14-0",
        "a delete carries the pre-image, all the way to the zone"
    );
}

// ── who is shipped, and who is not ──────────────────────────────────────────

#[test]
fn a_tombstoned_tenant_is_not_shipped_and_is_not_an_anomaly() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    add_holdings(&world.collection(ALICE), 3, "2026-08-14");
    add_holdings(&world.collection(BOB), 3, "2026-08-14");

    pkdump_keys::destroy::tombstone(&world.registry(), BOB, Some("account deleted")).unwrap();

    let report = ship(&world, &[alice, bob], 100);

    assert_eq!(
        report.outcome(),
        Outcome::Clean,
        "a revoked tenant is the system working, not a partial run"
    );
    assert_eq!(report.events(), 3, "only alice's");
    assert_eq!(report.tenants[1].status, Status::Revoked);
    assert!(
        world.objects().keys().all(|k| k.contains(ALICE)),
        "a tombstoned tenant's data reached the zone: {:?}",
        world.objects().keys().collect::<Vec<_>>()
    );
    // …and their outbox is untouched, so nothing was silently consumed.
    assert_eq!(cursor::shipped_thru(&world.collection(BOB)).unwrap(), 0);
}

#[test]
fn an_unregistered_tenant_is_skipped_and_named_rather_than_shipped() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    // Absence is not permission: bob exists, but nothing is recorded about
    // his key. This is what a registry restored without its rows looks like.
    world
        .registry()
        .execute(
            "DELETE FROM tenant_key WHERE database_id = ?1",
            rusqlite::params![BOB],
        )
        .unwrap();
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    add_holdings(&world.collection(BOB), 2, "2026-08-14");

    let report = ship(&world, &[alice, bob], 100);

    assert_eq!(report.outcome(), Outcome::Partial);
    assert_eq!(report.outcome().code(), 2);
    let skipped = report.skipped();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].0, BOB);
    assert!(
        skipped[0].1.contains("pkdump keys register"),
        "the skip must say what to do about it: {}",
        skipped[0].1
    );
    assert!(world.objects().keys().all(|k| k.contains(ALICE)));
}

#[test]
fn a_run_that_ships_nobody_at_all_is_a_failure_not_a_warning() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    // The systemic failure: the master key is gone, so no tenant can ship.
    std::fs::remove_file(pkdump_keys::master::key_path().unwrap()).unwrap();

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Failed);
    assert_eq!(report.outcome().code(), 1);
    assert!(
        report.skipped()[0].1.contains("OPERATIONAL FAILURE"),
        "a missing key must never read as a deletion: {}",
        report.skipped()[0].1
    );
    assert!(world.objects().is_empty());
}

#[test]
fn a_registered_tenant_with_no_database_is_skipped_rather_than_invented() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    add_holdings(&world.collection(ALICE), 2, "2026-08-14");
    std::fs::remove_file(&bob.path).unwrap();

    let report = ship(&world, &[alice, bob], 100);
    assert_eq!(report.outcome(), Outcome::Partial);
    assert!(report.skipped()[0].1.contains("no database at"));
}

#[test]
fn a_collection_with_an_empty_outbox_ships_nothing_and_is_clean() {
    // pd-whsw's shape: an existing collection whose outbox starts empty. The
    // shipper has nothing to say about it — the backfill (item 5) is what
    // puts those holdings through the outbox, and it does so as ordinary
    // events this code cannot tell apart from any other.
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-08-14");
    conn.execute("DELETE FROM ownership_outbox", []).unwrap();

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(report.events(), 0);
    assert!(
        report.gaps().is_empty(),
        "an outbox that was never written is not a gap — nothing was lost, \
         it was never there. A gap needs a cursor that has already passed \
         numbers these rows should have followed."
    );
    assert!(world.objects().is_empty());
}

/// ...and the other half of that shape: once the backfill HAS run, those
/// holdings ship, as ordinary events.
///
/// This is the seam between item 5 (`pd-385w`) and item 4 — the two halves of
/// arming the shipper on a box that already holds cards. Both crates state
/// the claim ("the shipper must NOT branch on provenance"; "every row is
/// shipped identically") and until they lived on one branch neither could
/// show it: `pkdump-db`'s own gate compares a backfill against a stand-in
/// projection, never against the zone the shipper writes.
///
/// The fixture is a collection whose rows PREDATE the triggers, which is
/// every existing box: the events are absent and so is the sequence they
/// would have burned. Reset one without the other and the backfill's first
/// event arrives above a cursor that never passed anything, which is a real
/// gap and would be reported as one.
#[test]
fn a_backfilled_collection_ships_as_ordinary_events() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let mut conn = world.collection(ALICE);
    add_holdings(&conn, 3, "2026-03-04");
    conn.execute("DELETE FROM ownership_outbox", []).unwrap();
    conn.execute(
        "DELETE FROM sqlite_sequence WHERE name = 'ownership_outbox'",
        [],
    )
    .unwrap();

    let emitted = pkdump_db::outbox::emit(&mut conn, &pkdump_db::outbox::Scope::Collection, false)
        .expect("the backfill runs");
    assert_eq!(emitted.events, 3);

    let report = ship(&world, &[alice], 100);
    assert_eq!(report.outcome(), Outcome::Clean);
    assert_eq!(report.events(), 3);
    assert!(
        report.gaps().is_empty(),
        "the backfill numbered from 1 and nothing was lost"
    );

    let (key, bytes) = world.objects().into_iter().next().unwrap();
    assert!(
        key.contains("as_of=2026-03-04"),
        "a backfilled event is dated from the row's own acquired_at, not from \
         the day the backfill ran — otherwise re-running it would move the \
         partition. Key was {key}"
    );
    let tenant_key = pkdump_keys::tenant_key(&world.registry(), ALICE).unwrap();
    let events = encode::decode(cipher::open(&tenant_key, &key, &bytes).unwrap()).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| (e.source_table.as_str(), e.op.as_str()))
            .collect::<Vec<_>>(),
        [("collection", "insert"); 3],
        "the zone holds the holdings, described exactly as a trigger would \
         have described them"
    );
    let shipped: Vec<String> = events
        .iter()
        .map(|e| {
            serde_json::from_str::<serde_json::Value>(&e.payload).unwrap()["printing_id"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        shipped,
        (0..3)
            .map(|i| format!("fixture-2026-03-04-{i}"))
            .collect::<Vec<_>>()
    );

    // And it is over: a second run has nothing to add, exactly as it would
    // have after an ordinary night's shipping.
    let before = world.objects();
    let again = ship(&world, &pkdump_db::tenants::list().unwrap(), 100);
    assert_eq!(again.events(), 0);
    assert_eq!(world.objects(), before);
}
