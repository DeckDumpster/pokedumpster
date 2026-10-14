//! Cloudflare Access JWT validation — the application-layer authentication gate.
//!
//! The verification itself — RS256 only, issuer, audience, `exp` and `nbf` checked, the JWKS
//! cache and its refresh rate limit, the proof-carrying [`VerifiedIdentity`] — is the tenancy
//! kernel's (`dumpster_kit_tenancy::access`), shared with every other site and held to its
//! negative-token corpus. What is PokeDumpster's is how it is **switched on**.
//!
//! # Opt-in by configuration
//!
//! The layer is **opt-in**: setting all three env vars below enables it. When none are set the
//! server starts without authentication — suitable for CI, tests, and local development. A
//! partial configuration (some but not all vars set) is a misconfiguration and the server
//! refuses to start. (The kernel itself has no mode without Access; that this site has one is
//! its single-tenant deployment, which [`crate::tenant`] keeps.)
//!
//! When the layer is active it is completely fail-closed: a missing or invalid token returns
//! 401 with no fallthrough. There is no `PKDUMP_AUTH_DISABLED`, no skip-on-loopback, and no
//! debug mode.
//!
//! * `PKDUMP_ACCESS_TEAM_DOMAIN` — full team URL, e.g. `https://myteam.cloudflareaccess.com`
//! * `PKDUMP_ACCESS_AUD` — the Access application's audience tag
//! * `PKDUMP_ACCESS_JWKS_URL` — the JWKS endpoint, **explicit** rather than derived from the
//!   team domain so that test infrastructure can point at a localhost server
//!
//! When configured, the JWKS is fetched at startup. A failed fetch is a failed startup.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use dumpster_kit_tenancy::access as kit;

pub use dumpster_kit_tenancy::access::{AccessConfig, AccessState, JWT_HEADER, VerifiedIdentity};

use crate::{AppError, AppState};

pub const TEAM_DOMAIN_ENV: &str = "PKDUMP_ACCESS_TEAM_DOMAIN";
pub const AUD_ENV: &str = "PKDUMP_ACCESS_AUD";
pub const JWKS_URL_ENV: &str = "PKDUMP_ACCESS_JWKS_URL";

/// Read the three env vars only when at least one is present in the environment. `None` when
/// none are set — Access is not configured, and the server starts without the authentication
/// layer. `Err` when at least one is set but the set is incomplete: a partial configuration is a
/// misconfiguration, not a quiet skip.
///
/// This is the configuration-as-enablement idiom: setting up the config IS enabling the layer.
/// There is no separate opt-in flag.
pub fn config_if_configured() -> anyhow::Result<Option<AccessConfig>> {
    let any_set = [TEAM_DOMAIN_ENV, AUD_ENV, JWKS_URL_ENV]
        .iter()
        .any(|v| std::env::var(v).is_ok());
    if !any_set {
        return Ok(None);
    }
    Ok(Some(AccessConfig::from_env(pkdump_db::app_prefix())?))
}

/// The identity verified for the request being served on this task.
pub(crate) fn current() -> Result<VerifiedIdentity, AppError> {
    Ok(kit::current()?)
}

/// Middleware: verify the Cloudflare Access JWT, then run the rest of the request with the
/// identity in scope. Applied to `/api` as a `route_layer`.
///
/// When `state.access` is `None` (Access not configured, single-tenant mode only —
/// `check_multitenant_access` refuses multi-tenant without Access) the request passes through
/// and no identity is in scope: single-tenant resolution never asks for one.
///
/// When the layer IS active it is the kernel's, and completely fail-closed — no token means 401.
pub(crate) async fn layer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let Some(access) = state.access.clone() else {
        return Ok(next.run(request).await);
    };
    Ok(kit::layer(State(access), request, next).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three variable names are spelled here because lib.rs and the operator docs name them;
    /// they are the kernel's derivation from the prefix, and must stay so.
    #[test]
    fn the_variables_are_the_kernels_derivation_from_the_prefix() {
        let app = pkdump_db::app_prefix();
        assert_eq!(TEAM_DOMAIN_ENV, app.var(kit::TEAM_DOMAIN_SUFFIX));
        assert_eq!(AUD_ENV, app.var(kit::AUD_SUFFIX));
        assert_eq!(JWKS_URL_ENV, app.var(kit::JWKS_URL_SUFFIX));
    }
}
