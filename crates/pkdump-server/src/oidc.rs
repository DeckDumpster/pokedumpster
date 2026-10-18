//! Native sign-in against Pocket ID, through dumpster-kit's OIDC module.
//!
//! Opt-in by configuration, exactly as [`crate::access`] is: setting any `PKDUMP_OIDC_*`
//! variable enables it, and an incomplete set is a startup failure rather than a quiet skip.
//! Unconfigured, the app serves as it always has (CI, tests, local development).
//!
//! When on, every human path — the SPA, its assets and `/api` — requires a session for a member
//! of `PKDUMP_OIDC_REQUIRED_GROUP` (`poke-users`). The machine paths in [`MACHINE_PATHS`] are
//! served without one, and are the only ones.
//!
//! # The automation paths
//!
//! * `/health` — liveness; returns `ok`.
//! * `/api/backup-status` — backup freshness timestamps, read by `alarm-status.sh` from plain
//!   localhost. It was already the one `/api` route outside authentication
//!   ([`crate::routes::public_api_router`]).
//!
//! Nothing here carries a Cloudflare service token, so there is no Access app to keep narrow and
//! no bearer token to mint: both paths expose no tenant data, were already unauthenticated, and
//! stay so. Anything added to this list must meet the same bar.
//!
//! The heartbeat (`deploy/heartbeat.sh`) probes `PKDUMP_HEARTBEAT_URL` and wants a 200; `/` now
//! answers a redirect to sign-in, so that host setting should name `/health`.

use std::sync::Arc;

use dumpster_kit_oidc::{Oidc, OidcConfig};

pub use dumpster_kit_oidc::SessionUser;

/// Path prefixes served without a session. See the module docs.
pub const MACHINE_PATHS: [&str; 2] = ["/health", "/api/backup-status"];

const PREFIX: &str = "PKDUMP";

/// The configuration, when any `PKDUMP_OIDC_*` variable is present. `Err` for an incomplete set.
pub fn config_if_configured() -> anyhow::Result<Option<OidcConfig>> {
    config_from(|k| std::env::var(k).ok(), std::env::vars().map(|(k, _)| k))
}

fn config_from(
    get: impl Fn(&str) -> Option<String>,
    names: impl Iterator<Item = String>,
) -> anyhow::Result<Option<OidcConfig>> {
    let marker = format!("{PREFIX}_OIDC_");
    if !names.into_iter().any(|n| n.starts_with(&marker)) {
        return Ok(None);
    }
    let mut cfg = OidcConfig::from_lookup(PREFIX, get)?;
    for p in MACHINE_PATHS {
        if !cfg.bypass_paths.iter().any(|b| b == p) {
            cfg.bypass_paths.push(p.to_string());
        }
    }
    Ok(Some(cfg))
}

/// Fetch discovery and prime the JWKS. A failure is a failed startup.
pub async fn connect(cfg: OidcConfig) -> anyhow::Result<Arc<Oidc>> {
    Ok(Oidc::discover(cfg).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn nothing_set_is_not_configured() {
        let r = config_from(lookup(&[]), ["PATH".to_string()].into_iter()).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn a_partial_set_is_a_startup_error_naming_what_is_missing() {
        let err = config_from(
            lookup(&[("PKDUMP_OIDC_ISSUER", "https://id.example")]),
            ["PKDUMP_OIDC_ISSUER".to_string()].into_iter(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("PKDUMP_OIDC_CLIENT_ID"), "{err}");
    }

    #[test]
    fn a_full_set_always_carries_the_machine_paths() {
        let cfg = config_from(
            lookup(&[
                ("PKDUMP_OIDC_ISSUER", "https://id.example"),
                ("PKDUMP_OIDC_CLIENT_ID", "c"),
                ("PKDUMP_OIDC_CLIENT_SECRET", "s"),
                (
                    "PKDUMP_OIDC_REDIRECT_URL",
                    "https://poke.example/auth/callback",
                ),
                ("PKDUMP_OIDC_REQUIRED_GROUP", "poke-users"),
                (
                    "PKDUMP_OIDC_COOKIE_KEY",
                    "0000000000000000000000000000000000000000000000000000000000000000",
                ),
            ]),
            ["PKDUMP_OIDC_ISSUER".to_string()].into_iter(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(cfg.required_group, "poke-users");
        for p in MACHINE_PATHS {
            assert!(cfg.bypass_paths.iter().any(|b| b == p), "{p}");
        }
    }
}
