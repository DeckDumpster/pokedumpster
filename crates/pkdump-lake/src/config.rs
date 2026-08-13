//! Where the lake lives — read, never guessed.
//!
//! The bucket is **host configuration**, not a repo constant, exactly like
//! the Litestream bucket and the container store before it. It comes from
//! `~/.config/pkdump/lake.env` in the same dotenv shape as `alerts.env`,
//! `litestream.env` and `store.env`, and an explicit environment variable
//! beats the file (the `store.env` precedent).
//!
//! If it is not configured, landing **refuses and names the file**. It does
//! not fall back to a default bucket, and it does not quietly skip the
//! landing step and let a refresh report success — the whole point of the
//! landing zone is that the bytes are there afterwards, so "configured
//! wrong" and "did nothing" must not look alike.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{LakeError, Result};

/// The bucket name. Host config; there is no default and never will be.
pub const KEY_BUCKET: &str = "PKDUMP_LAKE_S3_BUCKET";
/// The bucket's region.
pub const KEY_REGION: &str = "PKDUMP_LAKE_S3_REGION";
/// A key prefix inside the bucket. Optional; empty means `raw/` sits at the
/// bucket root.
pub const KEY_PREFIX: &str = "PKDUMP_LAKE_S3_PREFIX";
/// An S3 endpoint override — how a MinIO stands in for the real bucket.
pub const KEY_ENDPOINT: &str = "PKDUMP_LAKE_S3_ENDPOINT";
/// A local directory to land into instead of a bucket. Test tier only; it
/// takes precedence over the S3 settings when both are present.
pub const KEY_DIR: &str = "PKDUMP_LAKE_DIR";
/// Redirects the config file itself, so a test never reads — or writes —
/// the operator's real one (the `PKDUMP_ALERTS_ENV` precedent).
pub const KEY_ENV_FILE: &str = "PKDUMP_LAKE_ENV";

/// The default location of the config file, relative to `$HOME`.
pub const CONFIG_RELATIVE_PATH: &str = ".config/pkdump/lake.env";

/// Where landed objects go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Backend {
    /// The real thing.
    S3 {
        /// Bucket name.
        bucket: String,
        /// Bucket region.
        region: String,
        /// Key prefix, possibly empty.
        prefix: String,
        /// Endpoint override, for a MinIO stand-in.
        endpoint: Option<String>,
    },
    /// A directory on disk — hermetic gates only.
    Dir(PathBuf),
}

/// A resolved landing-zone configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LakeConfig {
    /// Where to write.
    pub backend: Backend,
}

impl LakeConfig {
    /// Resolve from the process environment layered over
    /// `~/.config/pkdump/lake.env`, environment winning.
    ///
    /// Fails, naming the file, when nothing configures a destination.
    pub fn load() -> Result<Self> {
        let path = config_path();
        let mut settings = match &path {
            Some(p) => read_env_file(p)?,
            None => BTreeMap::new(),
        };
        for key in [KEY_BUCKET, KEY_REGION, KEY_PREFIX, KEY_ENDPOINT, KEY_DIR] {
            if let Ok(value) = std::env::var(key) {
                settings.insert(key.to_string(), value);
            }
        }
        Self::from_settings(&settings, path.as_deref())
    }

    /// Resolve from an explicit settings map. `config_path` only shapes the
    /// error message — it is the file an operator would have to write.
    pub fn from_settings(
        settings: &BTreeMap<String, String>,
        config_path: Option<&Path>,
    ) -> Result<Self> {
        let get = |k: &str| {
            settings
                .get(k)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };

        if let Some(dir) = get(KEY_DIR) {
            return Ok(Self {
                backend: Backend::Dir(PathBuf::from(dir)),
            });
        }

        let where_from = match config_path {
            Some(p) => p.display().to_string(),
            None => format!("~/{CONFIG_RELATIVE_PATH}"),
        };

        let (Some(bucket), Some(region)) = (get(KEY_BUCKET), get(KEY_REGION)) else {
            return Err(LakeError::NotConfigured(format!(
                "the raw landing zone is not configured: {where_from} is missing or does not set \
                 {KEY_BUCKET} and {KEY_REGION}.\n\
                 The lake bucket is host configuration and has no default — it is deliberately a \
                 separate bucket from the Litestream backup bucket, because the backups are the \
                 only irreplaceable data in the system and a lifecycle rule written for one must \
                 never be able to reach the other.\n\
                 Create the bucket, then write {where_from}:\n\
                 \x20 {KEY_BUCKET}=<bucket>\n\
                 \x20 {KEY_REGION}=<region>\n\
                 \x20 #{KEY_PREFIX}=          # optional key prefix\n\
                 \x20 #{KEY_ENDPOINT}=        # optional, for a MinIO stand-in"
            )));
        };

        Ok(Self {
            backend: Backend::S3 {
                bucket,
                region,
                prefix: get(KEY_PREFIX).unwrap_or_default(),
                endpoint: get(KEY_ENDPOINT),
            },
        })
    }
}

/// The config file this process would read, honouring [`KEY_ENV_FILE`].
/// `None` when there is no `$HOME` to resolve the default against.
pub fn config_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var(KEY_ENV_FILE) {
        return Some(PathBuf::from(explicit));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(CONFIG_RELATIVE_PATH))
}

/// Parse a dotenv file. Missing is not an error — it is simply no settings,
/// which the caller turns into the refusal above.
pub(crate) fn read_env_file(path: &Path) -> Result<BTreeMap<String, String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parse_env(&text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e.into()),
    }
}

/// The same subset of dotenv the deploy scripts' `set -a; . file` handles:
/// `KEY=value`, `#` comments, blank lines, an optional `export ` prefix, and
/// optional surrounding quotes.
pub(crate) fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        out.insert(key.trim().to_string(), value.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_the_deploy_scripts_dotenv_subset() {
        let parsed = parse_env(
            "# the lake bucket\n\
             \n\
             PKDUMP_LAKE_S3_BUCKET=pkdump-lake-1\n\
             export PKDUMP_LAKE_S3_REGION=\"us-west-2\"\n\
             PKDUMP_LAKE_S3_PREFIX='sub/dir'\n\
             #PKDUMP_LAKE_S3_ENDPOINT=\n",
        );
        assert_eq!(parsed.get(KEY_BUCKET).unwrap(), "pkdump-lake-1");
        assert_eq!(parsed.get(KEY_REGION).unwrap(), "us-west-2");
        assert_eq!(parsed.get(KEY_PREFIX).unwrap(), "sub/dir");
        assert!(!parsed.contains_key(KEY_ENDPOINT));
    }

    #[test]
    fn resolves_an_s3_backend() {
        let cfg = LakeConfig::from_settings(
            &settings(&[
                (KEY_BUCKET, "pkdump-lake-1"),
                (KEY_REGION, "us-west-2"),
                (KEY_PREFIX, "lake"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.backend,
            Backend::S3 {
                bucket: "pkdump-lake-1".into(),
                region: "us-west-2".into(),
                prefix: "lake".into(),
                endpoint: None,
            }
        );
    }

    /// The bead's standing decision, executable: an absent or half-written
    /// `lake.env` refuses and says which file to write. Nothing is invented.
    #[test]
    fn an_unconfigured_lake_refuses_and_names_the_file() {
        let err = LakeConfig::from_settings(
            &BTreeMap::new(),
            Some(Path::new("/home/someone/.config/pkdump/lake.env")),
        )
        .unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("/home/someone/.config/pkdump/lake.env"),
            "{text}"
        );
        assert!(text.contains(KEY_BUCKET), "{text}");
        assert!(!text.to_lowercase().contains("default bucket"), "{text}");

        // A region with no bucket is just as unconfigured.
        assert!(LakeConfig::from_settings(&settings(&[(KEY_REGION, "us-west-2")]), None).is_err());
        // …and an empty value is not a value.
        assert!(
            LakeConfig::from_settings(
                &settings(&[(KEY_BUCKET, "  "), (KEY_REGION, "us-west-2")]),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn a_directory_backend_wins_over_s3_settings() {
        let cfg = LakeConfig::from_settings(
            &settings(&[
                (KEY_DIR, "/tmp/lake"),
                (KEY_BUCKET, "pkdump-lake-1"),
                (KEY_REGION, "us-west-2"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.backend, Backend::Dir(PathBuf::from("/tmp/lake")));
    }

    #[test]
    fn a_missing_file_is_no_settings_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let parsed = read_env_file(&tmp.path().join("absent.env")).unwrap();
        assert!(parsed.is_empty());
    }
}
