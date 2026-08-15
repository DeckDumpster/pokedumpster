//! The deletion path over real shipped data — `pd-qbrf`'s claim, end to end.
//!
//! The unit tests in the crate seed the zone by hand, which is enough to
//! prove a prefix is emptied but not enough to prove that what was emptied
//! was ever a tenant's holdings. So these start further back: a collection
//! database with rows in it, the outbox triggers that record them, the real
//! shipper writing real sealed parts, and only then a deletion.
//!
//! | claim                                          | test                                        |
//! |------------------------------------------------|---------------------------------------------|
//! | shipped holdings are unreachable after deletion | `a_deleted_tenant_s_shipped_holdings…`     |
//! | …including a copy that survived the drop        | `a_copy_that_survived_the_drop…`            |
//! | SEEN RED: before deletion, every path is open   | `before_the_deletion_every_one_of_those…`   |
//! | valuations go with the holdings                 | `a_deletion_takes_the_valuations…`          |
//! | one tenant's deletion is one tenant's           | `deleting_one_tenant_leaves_the_other…`     |
//! | a deleted tenant never ships again              | `a_deleted_tenant_is_not_shipped_again`     |
//! | the online half is untouched                    | `the_deletion_touches_no_collection…`       |
//!
//! Every fixture is treated as if it were real tenant data — which is to say
//! there is none of it. The collections hold invented printing ids in a
//! throwaway data directory, and the zone is a directory that goes away with
//! the test.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use pkdump_erase::verify::Check;
use pkdump_erase::{StrayCopy, Verdict};
use pkdump_lake::{DirStore, ObjectPurge, TenantZoneConfig};

const ALICE: &str = "01J0000000000000000000000A";
const BOB: &str = "01J0000000000000000000000B";

/// The environment these jobs read is process-wide, so tests that set it
/// cannot overlap.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct World {
    _dir: tempfile::TempDir,
    _lock: MutexGuard<'static, ()>,
    home: PathBuf,
    zone: DirStore,
    config: TenantZoneConfig,
    previous_home: Option<String>,
    previous_key: Option<String>,
}

impl World {
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
            zone: DirStore::new(zone_root),
            config: TenantZoneConfig {
                profile: "pkdump-tenant-test".to_string(),
                prefix: pkdump_lake::TENANT_ROOT.to_string(),
            },
            previous_home,
            previous_key,
        }
    }

    fn registry(&self) -> rusqlite::Connection {
        pkdump_db::registry::open().unwrap()
    }

    /// Register `handle` under a chosen `database_id`, create their
    /// collection, and register their key.
    fn tenant(&self, handle: &str, database_id: &str) -> pkdump_db::tenants::Tenant {
        let registry = self.registry();
        registry
            .execute(
                "INSERT INTO user (database_id, handle, created_at, state) \
                 VALUES (?1, ?2, '2026-08-14T00:00:00Z', 'active')",
                rusqlite::params![database_id, handle],
            )
            .unwrap();
        let path = self
            .home
            .join("tenants")
            .join(format!("{database_id}.sqlite"));
        drop(pkdump_db::open_user(&path).unwrap());
        pkdump_keys::state::register(&registry, database_id).unwrap();

        pkdump_db::tenants::list()
            .unwrap()
            .into_iter()
            .find(|t| t.user.database_id == database_id)
            .expect("just created")
    }

    /// Add `n` holdings to a collection. The outbox triggers (`pd-5m54`) are
    /// what record them, so this needs no application code — which is also
    /// what makes the fixture honest about where an outbox row comes from.
    fn add_holdings(&self, tenant: &pkdump_db::tenants::Tenant, n: usize) {
        let conn = pkdump_db::open_user(&tenant.path).unwrap();
        for i in 0..n {
            conn.execute(
                "INSERT INTO collection (printing_id, acquired_at, source) \
                 VALUES (?1, '2026-08-14T00:00:00Z', 'fixture')",
                rusqlite::params![format!("invented-{}-{i}", tenant.user.handle)],
            )
            .unwrap();
        }
    }

    /// Run the real shipper over every tenant.
    fn ship(&self) -> pkdump_ship::Report {
        let tenants = pkdump_db::tenants::list().unwrap();
        pkdump_ship::ship_all(
            &self.zone,
            &self.config,
            &self.registry(),
            &tenants,
            pkdump_ship::run::DEFAULT_MAX_ROWS,
        )
    }

    /// Write a valuations object for `database_id` by hand.
    ///
    /// Phase 3 is items 6/7 and is deliberately not this item's business, but
    /// "derived artifacts inherit the deletion obligation" is, so the object
    /// has to be there to be deleted. Sealed under the tenant's real key, at
    /// the real key layout.
    fn seal_a_valuation(&self, database_id: &str) -> String {
        let object_key = self.config.rooted(
            pkdump_lake::part_key(
                database_id,
                pkdump_lake::TenantDataset::Valuations,
                "2026-08-14",
                0,
            )
            .unwrap(),
        );
        let key = pkdump_keys::tenant_key(&self.registry(), database_id).unwrap();
        let sealed =
            pkdump_ship::cipher::seal(&key, &object_key, b"PAR1 what it was worth").unwrap();
        use pkdump_lake::ObjectStore;
        self.zone.put(&object_key, sealed).unwrap();
        object_key
    }

    /// Take a copy of one of a tenant's objects, exactly as it is in the
    /// zone — the "stray copy" a deletion has to be proven against.
    fn copy_out(&self, object_key: &str) -> StrayCopy {
        use pkdump_lake::ObjectSource;
        StrayCopy {
            object_key: object_key.to_string(),
            bytes: self.zone.get(object_key).unwrap(),
        }
    }

    fn keys_under(&self, prefix: &str) -> Vec<String> {
        ObjectPurge::list(&self.zone, prefix).unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let restore = |k: &str, v: &Option<String>| match v {
            Some(v) => unsafe { std::env::set_var(k, v) },
            None => unsafe { std::env::remove_var(k) },
        };
        restore("PKDUMP_HOME", &self.previous_home);
        restore(pkdump_keys::master::KEY_ENV_FILE, &self.previous_key);
    }
}

fn proof(verdict: &Verdict, check: Check) -> &pkdump_erase::Proof {
    verdict
        .proofs
        .iter()
        .find(|p| p.check == check)
        .unwrap_or_else(|| panic!("{check} was never checked"))
}

// ── the claim ───────────────────────────────────────────────────────────────

/// The headline: holdings that were really shipped are unreachable by every
/// path after a deletion.
#[test]
fn a_deleted_tenant_s_shipped_holdings_are_unreachable_by_every_path() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    world.tenant("bob", BOB);
    world.add_holdings(&alice, 6);
    assert_eq!(world.ship().events(), 6);

    let shipped = world.keys_under(&pkdump_lake::tenant_prefix(ALICE).unwrap());
    assert_eq!(shipped.len(), 1, "one part: {shipped:?}");
    let stray = world.copy_out(&shipped[0]);

    let done = pkdump_erase::delete(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        Some("account closed"),
        Some(&stray),
    )
    .unwrap();

    assert_eq!(done.dropped.count(), 1);
    assert!(
        done.verdict.proven(),
        "not proven: {:?}",
        done.verdict
            .failures()
            .iter()
            .map(|p| format!("{}: {}", p.check, p.detail))
            .collect::<Vec<_>>()
    );
    assert!(done.verdict.into_result().is_ok());
}

/// The check the crypto-shredding layer exists for. A byte-for-byte copy of a
/// real shipped part, taken before the deletion, survives the drop — and is
/// unopenable afterwards because there is no key left to open it.
#[test]
fn a_copy_that_survived_the_drop_is_ciphertext_nobody_holds_a_key_for() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    world.tenant("bob", BOB);
    world.add_holdings(&alice, 4);
    world.ship();

    let shipped = world.keys_under(&pkdump_lake::tenant_prefix(ALICE).unwrap());
    let stray = world.copy_out(&shipped[0]);

    // Before: the copy opens, and the plaintext is a tenant's holdings.
    let key = pkdump_keys::tenant_key(&world.registry(), ALICE).unwrap();
    let plaintext = pkdump_ship::cipher::open(&key, &stray.object_key, &stray.bytes).unwrap();
    let events = pkdump_ship::encode::decode(plaintext).unwrap();
    assert_eq!(events.len(), 4);
    drop(key);

    pkdump_erase::delete(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        None,
        None,
    )
    .unwrap();

    // After: the bytes are exactly the same bytes, and nothing can open them.
    let err = pkdump_keys::tenant_key(&world.registry(), ALICE).unwrap_err();
    assert!(
        err.is_deliberate_revocation(),
        "the refusal must be a revocation, not a broken box: {err}"
    );

    let verdict = pkdump_erase::verify(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        Some(&stray),
    )
    .unwrap();
    assert!(proof(&verdict, Check::StrayCopy).held);
    assert!(verdict.proven());
}

/// SEEN RED. Every single one of those checks, run one moment earlier, must
/// report the path OPEN — and the stray-copy check must do it by actually
/// opening the copy rather than by inferring anything.
#[test]
fn before_the_deletion_every_one_of_those_checks_reports_the_path_open() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    world.tenant("bob", BOB);
    world.add_holdings(&alice, 4);
    world.ship();
    world.seal_a_valuation(ALICE);

    let shipped = world.keys_under(&pkdump_lake::tenant_prefix(ALICE).unwrap());
    let holdings = shipped
        .iter()
        .find(|k| k.contains("dataset=holdings"))
        .unwrap();
    let stray = world.copy_out(holdings);

    let verdict = pkdump_erase::verify(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        Some(&stray),
    )
    .unwrap();

    assert!(
        !verdict.proven(),
        "a live tenant must never verify as deleted"
    );
    assert!(
        proof(&verdict, Check::Machinery).held,
        "the box itself is fine — that is what makes the rest meaningful"
    );
    for open in [
        Check::Derivation,
        Check::Partition,
        Check::Dataset("holdings"),
        Check::Dataset("valuations"),
        Check::StrayCopy,
    ] {
        assert!(
            !proof(&verdict, open).held,
            "{open} should be OPEN before the deletion: {}",
            proof(&verdict, open).detail
        );
    }
    assert!(
        proof(&verdict, Check::StrayCopy).detail.contains("OPENED"),
        "the red direction must be a real attempt: {}",
        proof(&verdict, Check::StrayCopy).detail
    );
}

/// "Derived artifacts inherit the obligation." Valuations are tenant data and
/// go with one prefix, which is why `database_id` sits above `dataset=`.
#[test]
fn a_deletion_takes_the_valuations_with_the_holdings() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    world.tenant("bob", BOB);
    world.add_holdings(&alice, 3);
    world.ship();
    let valuation = world.seal_a_valuation(ALICE);

    let done = pkdump_erase::delete(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        None,
        None,
    )
    .unwrap();

    assert!(
        done.dropped.keys.contains(&valuation),
        "the valuation must be dropped by the same prefix: {:?}",
        done.dropped.keys
    );
    assert!(
        proof(&done.verdict, Check::Dataset("valuations")).held,
        "and be checked by name"
    );
}

/// One tenant's deletion is one tenant's deletion — the property a shared
/// master key makes worth asserting rather than assuming.
#[test]
fn deleting_one_tenant_leaves_the_others_shipped_data_readable() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    let bob = world.tenant("bob", BOB);
    world.add_holdings(&alice, 3);
    world.add_holdings(&bob, 5);
    world.ship();

    let bobs = world.keys_under(&pkdump_lake::tenant_prefix(BOB).unwrap());
    assert_eq!(bobs.len(), 1);
    let bobs_copy = world.copy_out(&bobs[0]);

    pkdump_erase::delete(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        None,
        None,
    )
    .unwrap();

    // Bob's objects are there, his key derives, and his part still opens.
    assert_eq!(
        world
            .keys_under(&pkdump_lake::tenant_prefix(BOB).unwrap())
            .len(),
        1
    );
    let key = pkdump_keys::tenant_key(&world.registry(), BOB).unwrap();
    let plaintext =
        pkdump_ship::cipher::open(&key, &bobs_copy.object_key, &bobs_copy.bytes).unwrap();
    assert_eq!(pkdump_ship::encode::decode(plaintext).unwrap().len(), 5);

    // …and the same verification says Bob is NOT deleted.
    let verdict =
        pkdump_erase::verify(&world.zone, &world.config, &world.registry(), BOB, None).unwrap();
    assert!(!verdict.proven());
}

/// The deletion has to stick against the thing that would otherwise undo it:
/// tomorrow night's shipper. A tombstoned tenant is not shipped, so new
/// holdings never reach a prefix that was dropped.
#[test]
fn a_deleted_tenant_is_not_shipped_again() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    world.tenant("bob", BOB);
    world.add_holdings(&alice, 3);
    world.ship();

    pkdump_erase::delete(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        Some("closed"),
        None,
    )
    .unwrap();

    // Rows keep arriving in the collection — the online side has not been
    // purged yet, which is the normal window between the two halves.
    world.add_holdings(&alice, 4);
    let report = world.ship();

    let hers = report
        .tenants
        .iter()
        .find(|t| t.database_id == ALICE)
        .unwrap();
    assert_eq!(
        hers.status,
        pkdump_ship::run::Status::Revoked,
        "a tombstoned tenant must be recognised as revoked, not skipped as an error"
    );
    assert_eq!(hers.parts, 0);
    assert!(
        world
            .keys_under(&pkdump_lake::tenant_prefix(ALICE).unwrap())
            .is_empty(),
        "the shipper put data back under a dropped prefix"
    );
}

/// The offline half only. Removing the collection database and releasing the
/// handle is `pkdump tenant purge`/`detach`, in the online CLI, deliberately
/// not here — see the crate docs.
#[test]
fn the_deletion_touches_no_collection_database_and_no_user_row() {
    let world = World::new();
    let alice = world.tenant("alice", ALICE);
    world.tenant("bob", BOB);
    world.add_holdings(&alice, 3);
    world.ship();

    let before = std::fs::read(&alice.path).unwrap();

    pkdump_erase::delete(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        None,
        None,
    )
    .unwrap();

    assert_eq!(
        std::fs::read(&alice.path).unwrap(),
        before,
        "the tenant's own database must be byte-identical: the online half is another command"
    );
    assert!(
        pkdump_db::registry::find(&world.registry(), ALICE)
            .unwrap()
            .is_some(),
        "the user row is not this command's to remove"
    );
}

/// A tenant who never shipped anything is deleted successfully, and proven.
/// Deletion must not depend on the tenant having had data.
#[test]
fn a_tenant_who_never_shipped_is_still_deleted_and_still_proven() {
    let world = World::new();
    world.tenant("alice", ALICE);
    world.tenant("bob", BOB);

    let done = pkdump_erase::delete(
        &world.zone,
        &world.config,
        &world.registry(),
        ALICE,
        Some("closed before they used it"),
        None,
    )
    .unwrap();
    assert_eq!(done.dropped.count(), 0);
    assert!(done.verdict.proven());
}
