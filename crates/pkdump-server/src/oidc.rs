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
//!
//! # Closing `/health` (sp-8pjx8m)
//!
//! With `PKDUMP_HEALTH_REQUIRE_AUTH=1` (default `0`) `/health` leaves [`MACHINE_PATHS`] and needs
//! one of: a session in the required group; a Pocket ID `client_credentials` bearer token whose
//! client id is on `PKDUMP_OIDC_BEARER_CLIENT_IDS` (see [`Machine`]); or a loopback peer, for the
//! host-local deploy check. A bearer token reaches `/health` and nothing else: on any other path
//! it is a 401, so a machine credential can never read user data.
//!
//! Loopback means the TCP peer is loopback **and** the request carries no forwarding header.
//! cloudflared connects from the host's loopback, so the peer address alone would admit the
//! whole internet.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dumpster_kit_oidc::jwks::JwksCache;
use dumpster_kit_oidc::{Oidc, OidcConfig};
use jsonwebtoken::{Algorithm, Validation};
use serde::Deserialize;

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
    let health_closed = flag_from(&get, HEALTH_FLAG)?;
    let mut cfg = OidcConfig::from_lookup(PREFIX, get)?;
    for p in MACHINE_PATHS {
        if health_closed && p == "/health" {
            continue;
        }
        if !cfg.bypass_paths.iter().any(|b| b == p) {
            cfg.bypass_paths.push(p.to_string());
        }
    }
    Ok(Some(cfg))
}

const HEALTH_FLAG: &str = "PKDUMP_HEALTH_REQUIRE_AUTH";
const BEARER_IDS: &str = "PKDUMP_OIDC_BEARER_CLIENT_IDS";
const BEARER_AUDIENCE: &str = "PKDUMP_OIDC_BEARER_AUDIENCE";

/// `0` or `1`; unset is `0`. Anything else is a startup error, because a typo on a switch that
/// opens or closes a route must not be read as either.
fn flag_from(get: &impl Fn(&str) -> Option<String>, name: &str) -> anyhow::Result<bool> {
    match get(name).as_deref().map(str::trim) {
        None | Some("") | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(v) => anyhow::bail!("{name} must be 0 or 1, got {v:?}"),
    }
}

/// Machine-caller settings: whether `/health` is closed, and which Pocket ID clients may reach it.
#[derive(Clone, Debug, Default)]
pub struct MachineConfig {
    pub health_requires_auth: bool,
    pub client_ids: Vec<String>,
    pub audience: Option<String>,
}

pub fn machine_config() -> anyhow::Result<MachineConfig> {
    machine_config_from(|k| std::env::var(k).ok())
}

pub(crate) fn machine_config_from(
    get: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<MachineConfig> {
    let client_ids: Vec<String> = get(BEARER_IDS)
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let audience = get(BEARER_AUDIENCE)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if !client_ids.is_empty() && audience.is_none() {
        anyhow::bail!("{BEARER_IDS} is set, so {BEARER_AUDIENCE} is required");
    }
    Ok(MachineConfig {
        health_requires_auth: flag_from(&get, HEALTH_FLAG)?,
        client_ids,
        audience,
    })
}

/// Verifier for `client_credentials` access tokens, and the `/health` gate built on it.
pub struct Machine {
    cfg: MachineConfig,
    issuer: String,
    required_group: String,
    jwks: Option<JwksCache>,
}

#[derive(Deserialize)]
struct Discovery {
    jwks_uri: String,
}

#[derive(Deserialize)]
struct TokenClaims {
    azp: Option<String>,
    client_id: Option<String>,
    sub: Option<String>,
}

impl Machine {
    /// Fetch discovery and prime the JWKS when any client is allow-listed. A failure is a
    /// failed startup.
    pub async fn connect(
        cfg: MachineConfig,
        issuer: &str,
        required_group: &str,
    ) -> anyhow::Result<Arc<Machine>> {
        let issuer = issuer.trim_end_matches('/').to_string();
        let jwks = if cfg.client_ids.is_empty() {
            None
        } else {
            let http = reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()?;
            let url = format!("{issuer}/.well-known/openid-configuration");
            let disc: Discovery = http
                .get(&url)
                .send()
                .await
                .and_then(|r| r.error_for_status())?
                .json()
                .await?;
            let cache = JwksCache::new(disc.jwks_uri, http, Duration::from_secs(60));
            cache.prime().await?;
            Some(cache)
        };
        Ok(Arc::new(Machine {
            cfg,
            issuer,
            required_group: required_group.to_string(),
            jwks,
        }))
    }

    /// The client id of a valid, unexpired token for our audience and issuer, if allow-listed.
    async fn admit(&self, token: &str) -> Option<String> {
        let jwks = self.jwks.as_ref()?;
        let audience = self.cfg.audience.as_deref()?;
        let kid = jsonwebtoken::decode_header(token).ok()?.kid?;
        let key = jwks.get(&kid).await?;
        let mut v = Validation::new(Algorithm::RS256);
        v.set_audience(&[audience]);
        v.set_issuer(&[self.issuer.as_str()]);
        v.set_required_spec_claims(&["exp", "aud", "iss"]);
        v.leeway = 60;
        let c = jsonwebtoken::decode::<TokenClaims>(token, &key, &v)
            .ok()?
            .claims;
        let id = c.azp.or(c.client_id).or(c.sub)?;
        self.cfg.client_ids.contains(&id).then_some(id)
    }

    fn synthetic_user(&self, id: String) -> dumpster_kit_oidc::SessionUser {
        dumpster_kit_oidc::SessionUser {
            sub: id.clone(),
            email: String::new(),
            name: id,
            groups: vec![self.required_group.clone()],
            exp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
                + 60,
        }
    }

    /// Outermost auth layer, wrapped around [`Oidc::layer`]. See the module docs.
    pub async fn layer(State(m): State<Arc<Machine>>, mut req: Request, next: Next) -> Response {
        let is_health = req.uri().path() == "/health";
        let bearer = req
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::to_string);
        if let Some(token) = bearer {
            if !is_health {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            return match m.admit(&token).await {
                Some(id) => {
                    req.extensions_mut().insert(m.synthetic_user(id));
                    next.run(req).await
                }
                None => StatusCode::UNAUTHORIZED.into_response(),
            };
        }
        if is_health && m.cfg.health_requires_auth {
            if is_loopback(&req) {
                req.extensions_mut()
                    .insert(m.synthetic_user("loopback".into()));
                return next.run(req).await;
            }
            let r = next.run(req).await;
            // The session layer answers a signed-out GET with a redirect to sign-in; a probe
            // has no browser to send there.
            return if r.status().is_redirection() {
                StatusCode::UNAUTHORIZED.into_response()
            } else {
                r
            };
        }
        next.run(req).await
    }
}

const FORWARDING_HEADERS: [&str; 5] = [
    "cf-connecting-ip",
    "cf-ray",
    "x-forwarded-for",
    "x-real-ip",
    "forwarded",
];

/// A loopback peer with no sign of a proxy. No peer address (a test harness, a future listener)
/// is not loopback.
fn is_loopback(req: &Request) -> bool {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|c| c.0.ip().is_loopback())
        && !FORWARDING_HEADERS
            .iter()
            .any(|h| req.headers().contains_key(*h))
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
