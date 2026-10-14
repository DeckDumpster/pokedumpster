//! The bytes already on disk and in the bucket still open.
//!
//! The cutover replaced the code that derives every tenant's key and seals every part in the
//! tenant zone. Nothing may change about what that code produces for input it has already
//! seen: a derivation that drifts by one byte is every shipped part unreadable, and nothing
//! reports it — a part that will not open looks exactly like a part that was never written.
//!
//! These vectors were produced by the pre-cutover implementation (`pkdump-keys` and
//! `pkdump-ship`'s own `cipher`, at 4fd4d66) from a fixed master key, and are asserted here
//! against the kit's implementation bound by `pkdump_site`. They are literals on purpose: a
//! test that computed the expected value with the code under test would pass for any prefix.

use std::os::unix::fs::PermissionsExt;

use pkdump_ship::cipher;

const ID: &str = "01J0000000000000000000000A";
const OBJECT: &str = "tenant/database_id=01J0000000000000000000000A/dataset=holdings/\
                      as_of=2026-08-14/part-seq-000000000001-000000000002.parquet.enc";
const PLAINTEXT: &[u8] = b"PAR1 plaintext of a part";

const MASTER_FINGERPRINT: &str = "962881527a34401f";
const TENANT_KEY: &str = "69ed6eaf8ef7adcc8d8e3bebc320954bb74f1b0bc0318ce00c71ce51b5aa5b1b";
const TENANT_FINGERPRINT: &str = "5129fbde7202acbf";
const SEALED: &str = "504b44545a310a4f519cd44e34939180ef7623e9171a09fa2b4be098b641865d77500ccb7cd9c81cc663313f447f09c78c91b77e9f71042c9c63ca";

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The master key file a box has on disk, with the bytes `00 01 02 … 1f`.
fn with_master<T>(f: impl FnOnce(&rusqlite::Connection) -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tenant-master.key");
    let material: String = (0u8..32).map(|b| format!("{b:02x}")).collect();
    std::fs::write(
        &path,
        format!("# a comment\npkdump-master-key-v1:{material}\n"),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let var = pkdump_site::key_env().key_file_var;
    unsafe { std::env::set_var(&var, &path) };

    let registry = rusqlite::Connection::open_in_memory().unwrap();
    registry
        .execute_batch(include_str!("../../pkdump-db/src/schema_registry.sql"))
        .unwrap();
    dumpster_kit_keys::state::register(&registry, ID).unwrap();
    let out = f(&registry);
    unsafe { std::env::remove_var(&var) };
    out
}

#[test]
fn the_master_key_on_disk_has_the_fingerprint_it_always_had() {
    with_master(|_| {
        let fp = dumpster_kit_keys::derive::master_fingerprint(&pkdump_site::key_env()).unwrap();
        assert_eq!(fp, MASTER_FINGERPRINT);
    });
}

#[test]
fn a_tenant_key_derives_to_the_bytes_the_old_code_derived() {
    with_master(|registry| {
        let key = pkdump_ship::tenant_key(registry, ID).unwrap();
        assert_eq!(hex(key.as_bytes()), TENANT_KEY);
        assert_eq!(key.fingerprint(), TENANT_FINGERPRINT);
    });
}

#[test]
fn a_part_seals_to_the_bytes_the_old_code_sealed_and_the_old_bytes_still_open() {
    with_master(|registry| {
        let key = pkdump_ship::tenant_key(registry, ID).unwrap();
        let sealed = cipher::seal(&key, OBJECT, PLAINTEXT).unwrap();
        assert_eq!(
            hex(&sealed),
            SEALED,
            "a retry would no longer be byte-identical"
        );

        let old: Vec<u8> = (0..SEALED.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&SEALED[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(cipher::open(&key, OBJECT, &old).unwrap(), PLAINTEXT);
    });
}
