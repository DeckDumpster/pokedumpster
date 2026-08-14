//! Where landed objects go, and where a rebuild reads them back from.
//!
//! Two traits with two implementations each: [`S3Store`], which is the real
//! one, and [`DirStore`], a directory on disk that exists so the landing
//! zone's behaviour can be asserted hermetically — the key layout, the
//! manifest and the never-overwrite property are all properties of the
//! *keys*, and proving them against a bucket would make the gate need
//! credentials and a network.
//!
//! The traits are separate on purpose. [`ObjectStore`] is what a *landing*
//! run holds, and it is still write-only: a run knows its own key space and
//! must not be able to look at anybody else's. [`ObjectSource`] is what the
//! offline derive holds, and it is read-only for the mirror-image reason —
//! `raw/` is immutable, and a job that could PUT into it could rewrite the
//! evidence it exists to preserve. Nothing implements both halves *as one
//! handle*; the two are constructed from the same config by different
//! entry points ([`crate::open`] and [`crate::open_reader`]).
//!
//! [`ObjectPurge`] is the third, and it is narrower than either: enumerate a
//! prefix, and remove one key. It exists for exactly one caller — the tenant
//! zone's deletion sweep (`pd-qbrf`) — and it deliberately has no `get` and
//! no `put`, because a job whose business is *removing* a tenant's holdings
//! has none reading them and none adding to them. It is handed out only by
//! [`crate::open_tenant_zone_purge`], never by [`crate::open`].

use std::path::{Path, PathBuf};

use crate::error::{LakeError, Result};

/// A write-only object store, keyed by string. Landing never reads and never
/// lists — a run knows its own key space and nothing else's.
pub trait ObjectStore: Send + Sync {
    /// Store `body` at `key`, creating or replacing.
    ///
    /// Replacement is possible at this layer and prevented at the one above:
    /// keys carry a per-run ULID, so no two runs address the same key. This
    /// trait deliberately does not offer a conditional put — the guarantee
    /// lives in the layout, where it can be tested without a bucket.
    fn put(&self, key: &str, body: Vec<u8>) -> Result<()>;

    /// A short description of where this store writes, for progress output.
    fn describe(&self) -> String;
}

/// A read-only view of a landing zone, keyed by string.
///
/// The offline derive is the only caller. It needs exactly two operations —
/// list the immediate children of a prefix (to find the `run=` directories a
/// date holds) and read one object whole — and deliberately not a third:
/// there is no `put`, so a job that reads `raw/` cannot write to it.
pub trait ObjectSource: Send + Sync {
    /// The bytes stored at `key`.
    ///
    /// Missing is an error rather than `None`: every key this trait is asked
    /// for came out of a manifest, so an absent object means the landing zone
    /// and its own record of itself disagree.
    fn get(&self, key: &str) -> Result<Vec<u8>>;

    /// The names of the immediate child "directories" of `key_prefix`, in no
    /// particular order, with no trailing slash. A prefix that does not exist
    /// is empty rather than an error — "this date landed nothing" is a fact
    /// the caller turns into its own refusal, with the date in it.
    fn child_dirs(&self, key_prefix: &str) -> Result<Vec<String>>;

    /// A short description of where this source reads, for progress output.
    fn describe(&self) -> String;
}

/// Enumerate and remove objects. The deletion sweep's handle, and nothing
/// else's.
///
/// Two operations, both of which the other two traits deliberately lack:
/// [`ObjectStore`] cannot list (a landing run has no business seeing anybody
/// else's keys) and [`ObjectSource`] can only see a prefix's immediate child
/// *directories*, which is enough to find a date's runs and nowhere near
/// enough to find every object a tenant owns.
///
/// **This trait carries no scoping of its own.** `list` and `delete` will
/// address whatever key they are given; the prefix a caller is confined to is
/// the caller's business, and for the one caller there is that confinement is
/// `pkdump_erase::sweep`, which refuses a key outside the single tenant prefix
/// it was constructed with. Putting the guard there rather than here is
/// deliberate: a trait that tried to be safe would need to know what
/// `tenant/` means, and this module is about bytes and keys.
pub trait ObjectPurge: Send + Sync {
    /// Every object key under `key_prefix`, recursively, in sorted order.
    ///
    /// Recursive, unlike [`ObjectSource::child_dirs`]: a tenant's partition is
    /// two levels of `key=value` deep and the sweep's whole claim is that
    /// nothing under the prefix survives, which is not a claim any single
    /// level can make. A prefix holding nothing is an empty list rather than
    /// an error — "already gone" is the normal second run.
    fn list(&self, key_prefix: &str) -> Result<Vec<String>>;

    /// Remove one object. Removing a key that is not there is **not** an
    /// error: deletion is re-run after a crash, and a sweep that failed on
    /// the objects it had already removed could never finish.
    fn delete(&self, key: &str) -> Result<()>;

    /// A short description of where this handle deletes from.
    fn describe(&self) -> String;
}

/// A directory on the local filesystem, one file per key.
///
/// Test-tier only. It is not a lake — nothing reads from it — but it holds
/// exactly the bytes and exactly the keys the S3 store would.
pub struct DirStore {
    root: PathBuf,
}

impl DirStore {
    /// Write objects under `root`, which is created on demand.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory this store writes into.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl ObjectStore for DirStore {
    fn put(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let path = self.root.join(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, body)?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("dir {}", self.root.display())
    }
}

impl ObjectSource for DirStore {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.root.join(key))?)
    }

    fn child_dirs(&self, key_prefix: &str) -> Result<Vec<String>> {
        let dir = self.root.join(key_prefix);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        Ok(out)
    }

    fn describe(&self) -> String {
        format!("dir {}", self.root.display())
    }
}

impl ObjectPurge for DirStore {
    fn list(&self, key_prefix: &str) -> Result<Vec<String>> {
        // Walked rather than globbed, so a key is only ever built by joining
        // the components that were actually on disk — the alternative reads a
        // path back out of a string and gets to be wrong about separators.
        let mut out = Vec::new();
        walk(&self.root, key_prefix, &mut out)?;
        out.sort();
        Ok(out)
    }

    fn delete(&self, key: &str) -> Result<()> {
        match std::fs::remove_file(self.root.join(key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn describe(&self) -> String {
        format!("dir {}", self.root.display())
    }
}

/// Every file under `root/prefix`, as keys relative to `root`.
///
/// `prefix` is a key prefix rather than a directory: `tenant/database_id=X/`
/// is a directory, but the deletion sweep is entitled to be handed one that
/// is not, and a caller that had to know which is which would be a caller
/// that could be wrong about it.
fn walk(root: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let start = root.join(prefix);
    if start.is_file() {
        out.push(prefix.to_string());
        return Ok(());
    }
    let entries = match std::fs::read_dir(&start) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let base = prefix.trim_end_matches('/');
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let key = if base.is_empty() {
            name
        } else {
            format!("{base}/{name}")
        };
        if entry.file_type()?.is_dir() {
            walk(root, &format!("{key}/"), out)?;
        } else {
            out.push(key);
        }
    }
    Ok(())
}

/// The real landing zone: an S3 bucket.
///
/// Credentials come from the ambient AWS configuration chain — which, on
/// this project's boxes, means a profile whose `role_arn`/`source_profile`
/// the SDK assumes and **refreshes on its own**. That is the standing
/// credential decision for everything here, and it is why the AWS SDK is
/// worth its dependency weight over a hand-rolled SigV4 PUT: nothing in this
/// crate ever holds a long-lived key.
pub struct S3Store {
    client: aws_sdk_s3::Client,
    runtime: tokio::runtime::Runtime,
    bucket: String,
    prefix: String,
}

impl S3Store {
    /// Connect to `bucket` in `region`.
    ///
    /// `prefix` is prepended to every key (empty for none) and `endpoint`
    /// overrides the AWS endpoint, which is what lets a MinIO stand in for
    /// S3 in a container gate.
    pub fn connect(
        bucket: &str,
        region: &str,
        prefix: &str,
        endpoint: Option<&str>,
    ) -> Result<Self> {
        Self::connect_as(bucket, region, prefix, endpoint, None)
    }

    /// [`Self::connect`], under a named AWS profile.
    ///
    /// The tenant zone shares a bucket with the catalog and is separated from
    /// it by nothing but a pair of credential policies (`pd-uz8q`), so the
    /// identity a handle uses cannot be ambient — it is named, per handle, at
    /// the point the handle is built. Setting `AWS_PROFILE` in the process
    /// instead would make it a property of the *process*, and a process that
    /// touches both zones would then reach one of them with the other's role.
    ///
    /// Only the shared-config half of the chain is affected: an environment
    /// carrying explicit keys still wins, which is what lets a MinIO stand in
    /// for S3 in a gate that has no roles to assume.
    pub fn connect_as(
        bucket: &str,
        region: &str,
        prefix: &str,
        endpoint: Option<&str>,
        profile: Option<&str>,
    ) -> Result<Self> {
        // A current-thread runtime owned by the store: every caller in this
        // workspace's ingest path is blocking, and the alternative — making
        // the whole fetch path async for the sake of a PUT — would be a much
        // larger change than the feature warrants.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let region = aws_config::Region::new(region.to_string());
        let client = runtime.block_on(async {
            let mut loader =
                aws_config::defaults(aws_config::BehaviorVersion::latest()).region(region);
            if let Some(endpoint) = endpoint {
                loader = loader.endpoint_url(endpoint);
            }
            if let Some(profile) = profile {
                loader = loader.profile_name(profile);
            }
            let conf = loader.load().await;
            // Path-style addressing keeps a MinIO endpoint working; against
            // real S3 it is equivalent.
            let s3 = aws_sdk_s3::config::Builder::from(&conf)
                .force_path_style(endpoint.is_some())
                .build();
            aws_sdk_s3::Client::from_conf(s3)
        });

        Ok(Self {
            client,
            runtime,
            bucket: bucket.to_string(),
            prefix: normalize_prefix(prefix),
        })
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}{}", self.prefix, key)
    }
}

impl ObjectStore for S3Store {
    fn put(&self, key: &str, body: Vec<u8>) -> Result<()> {
        let key = self.full_key(key);
        self.runtime.block_on(async {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(body.into())
                .send()
                .await
                .map_err(|e| {
                    // The SDK's Display is a bare "service error"; the useful
                    // text is in the source chain.
                    LakeError::S3(format!(
                        "put s3://{}/{}: {}",
                        self.bucket,
                        key,
                        aws_error_text(&e)
                    ))
                })
        })?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("s3://{}/{}", self.bucket, self.prefix)
    }
}

impl ObjectSource for S3Store {
    fn get(&self, key: &str) -> Result<Vec<u8>> {
        let key = self.full_key(key);
        let body = self.runtime.block_on(async {
            let object = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    LakeError::S3(format!(
                        "get s3://{}/{}: {}",
                        self.bucket,
                        key,
                        aws_error_text(&e)
                    ))
                })?;
            object
                .body
                .collect()
                .await
                .map_err(|e| LakeError::S3(format!("read s3://{}/{}: {e}", self.bucket, key)))
        })?;
        Ok(body.into_bytes().to_vec())
    }

    fn child_dirs(&self, key_prefix: &str) -> Result<Vec<String>> {
        // `Delimiter=/` makes S3 answer with CommonPrefixes — the object-store
        // equivalent of "the immediate children", and the reason this does not
        // have to list every part under a date to find its runs.
        let prefix = self.full_key(key_prefix);
        self.runtime.block_on(async {
            let mut out = Vec::new();
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .delimiter("/")
                .into_paginator()
                .send();
            while let Some(page) = pages.next().await {
                let page = page.map_err(|e| {
                    LakeError::S3(format!(
                        "list s3://{}/{}: {}",
                        self.bucket,
                        prefix,
                        aws_error_text(&e)
                    ))
                })?;
                for common in page.common_prefixes() {
                    if let Some(p) = common.prefix() {
                        out.push(
                            p.trim_end_matches('/')
                                .rsplit('/')
                                .next()
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                }
            }
            Ok(out)
        })
    }

    fn describe(&self) -> String {
        format!("s3://{}/{}", self.bucket, self.prefix)
    }
}

impl ObjectPurge for S3Store {
    fn list(&self, key_prefix: &str) -> Result<Vec<String>> {
        // No `Delimiter` — the opposite of `child_dirs`. A tenant partition is
        // `database_id=/dataset=/as_of=/part-…`, so a delimited listing would
        // answer with the datasets and never with the objects, and a sweep
        // that deleted the answer would delete nothing at all.
        let prefix = self.full_key(key_prefix);
        let strip = self.prefix.clone();
        self.runtime.block_on(async {
            let mut out = Vec::new();
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix)
                .into_paginator()
                .send();
            while let Some(page) = pages.next().await {
                let page = page.map_err(|e| {
                    LakeError::S3(format!(
                        "list s3://{}/{}: {}",
                        self.bucket,
                        prefix,
                        aws_error_text(&e)
                    ))
                })?;
                for object in page.contents() {
                    let Some(key) = object.key() else { continue };
                    // Handed back in the caller's key space, not the store's:
                    // `S3Store` prepends its own configured prefix on the way
                    // in, and a key that came back still carrying it would be
                    // prefixed twice when passed to `delete`.
                    out.push(key.strip_prefix(&strip).unwrap_or(key).to_string());
                }
            }
            out.sort();
            Ok(out)
        })
    }

    fn delete(&self, key: &str) -> Result<()> {
        // S3's DELETE is already idempotent — removing an absent key answers
        // 204 — which is the behaviour this trait requires and the reason
        // there is no existence check here.
        let key = self.full_key(key);
        self.runtime.block_on(async {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    LakeError::S3(format!(
                        "delete s3://{}/{}: {}",
                        self.bucket,
                        key,
                        aws_error_text(&e)
                    ))
                })
        })?;
        Ok(())
    }

    fn describe(&self) -> String {
        format!("s3://{}/{}", self.bucket, self.prefix)
    }
}

/// Walk an SDK error's source chain — the outermost `Display` is rarely the
/// part that says what went wrong.
fn aws_error_text(err: &dyn std::error::Error) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    while let Some(e) = source {
        parts.push(e.to_string());
        source = e.source();
    }
    parts.join(": ")
}

/// A key prefix, normalized to either empty or `something/`.
fn normalize_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_store_creates_nested_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        store
            .put(
                "raw/source=tcgcsv/dataset=groups/x/part-0000.json.zst",
                b"hi".to_vec(),
            )
            .unwrap();
        assert_eq!(
            std::fs::read(
                tmp.path()
                    .join("raw/source=tcgcsv/dataset=groups/x/part-0000.json.zst")
            )
            .unwrap(),
            b"hi"
        );
    }

    /// The sweep's whole claim rests on this being recursive: a tenant
    /// partition is three `key=value` levels deep before an object appears.
    #[test]
    fn dir_store_lists_a_prefix_recursively_and_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        for key in [
            "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-0001",
            "tenant/database_id=A/dataset=holdings/as_of=2026-08-13/part-0000",
            "tenant/database_id=A/dataset=valuations/as_of=2026-08-14/part-0000",
            "tenant/database_id=B/dataset=holdings/as_of=2026-08-14/part-0000",
            "raw/source=tcgcsv/x",
        ] {
            store.put(key, b"x".to_vec()).unwrap();
        }

        assert_eq!(
            ObjectPurge::list(&store, "tenant/database_id=A/").unwrap(),
            vec![
                "tenant/database_id=A/dataset=holdings/as_of=2026-08-13/part-0000",
                "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-0001",
                "tenant/database_id=A/dataset=valuations/as_of=2026-08-14/part-0000",
            ],
            "one tenant's prefix must cover every dataset and no other tenant"
        );
    }

    /// A prefix nothing is under is an empty answer, not an error — the
    /// normal shape of the second run of a deletion.
    #[test]
    fn dir_store_lists_an_absent_prefix_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        assert!(
            ObjectPurge::list(&store, "tenant/database_id=NOBODY/")
                .unwrap()
                .is_empty()
        );
    }

    /// Deleting twice is what a resumed sweep does on every object it already
    /// removed. It has to be free.
    #[test]
    fn dir_store_deletes_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let store = DirStore::new(tmp.path());
        let key = "tenant/database_id=A/dataset=holdings/as_of=2026-08-14/part-0000";
        store.put(key, b"x".to_vec()).unwrap();
        store.delete(key).unwrap();
        store.delete(key).unwrap();
        assert!(ObjectPurge::list(&store, "tenant/").unwrap().is_empty());
    }

    #[test]
    fn prefix_normalizes_to_empty_or_trailing_slash() {
        assert_eq!(normalize_prefix(""), "");
        assert_eq!(normalize_prefix("   "), "");
        assert_eq!(normalize_prefix("/"), "");
        assert_eq!(normalize_prefix("lake"), "lake/");
        assert_eq!(normalize_prefix("/lake/"), "lake/");
        assert_eq!(normalize_prefix("a/b"), "a/b/");
    }
}
