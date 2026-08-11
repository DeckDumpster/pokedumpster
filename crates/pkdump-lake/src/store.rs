//! Where landed objects go.
//!
//! One trait with two implementations: [`S3Store`], which is the real one,
//! and [`DirStore`], a directory on disk that exists so the landing zone's
//! behaviour can be asserted hermetically — the key layout, the manifest and
//! the never-overwrite property are all properties of the *keys*, and
//! proving them against a bucket would make the gate need credentials and a
//! network.

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
