//! Cloudflare Access JWT validation — the application-layer authentication gate.
//!
//! When configured, every `/api` request must carry a valid Cloudflare Access
//! JWT, extracted from the `Cf-Access-Jwt-Assertion` header or the
//! `CF_Authorization` cookie. The token is verified against the team's published
//! JWKS: RS256 only, with issuer, audience, `exp`, and `nbf` all checked.
//!
//! # Opt-in by configuration
//!
//! The layer is **opt-in**: setting all three env vars below enables it. When
//! none are set the server starts without authentication — suitable for CI,
//! tests, and local development. A partial configuration (some but not all vars
//! set) is a misconfiguration and the server refuses to start.
//!
//! When the layer is active, it is completely fail-closed: a missing or invalid
//! token returns 401 with no fallthrough. There is no `PKDUMP_AUTH_DISABLED`,
//! no skip-on-loopback, and no debug mode. Tests use real RS256 keys through an
//! in-process JWKS server; they do not need a bypass.
//!
//! # Configuration
//!
//! * `PKDUMP_ACCESS_TEAM_DOMAIN` — full team URL, e.g.
//!   `https://myteam.cloudflareaccess.com`
//! * `PKDUMP_ACCESS_AUD` — the Access application's audience tag
//! * `PKDUMP_ACCESS_JWKS_URL` — the JWKS endpoint, **explicit** rather than
//!   derived from the team domain so that test infrastructure can point at a
//!   localhost server without faking the `iss` claim
//!
//! When configured, the JWKS is fetched at startup. A failed fetch is a failed
//! startup.
//!
//! # Key cache
//!
//! Keys are cached for 1 hour. An unknown `kid` triggers a re-fetch, but not
//! more than once per 60 seconds, so a stolen or fabricated token cannot be
//! used to hammer the JWKS endpoint.
//!
//! # `VerifiedIdentity`
//!
//! A proof-carrying type whose constructor is private. The only source is
//! [`layer`], which called [`AccessState::verify`]. A handler holding one —
//! obtained via [`current`] — is guaranteed the request was authenticated.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use tokio::sync::RwLock;

use crate::{AppError, AppState};

// ---- public constants -------------------------------------------------------

pub const TEAM_DOMAIN_ENV: &str = "PKDUMP_ACCESS_TEAM_DOMAIN";
pub const AUD_ENV: &str = "PKDUMP_ACCESS_AUD";
pub const JWKS_URL_ENV: &str = "PKDUMP_ACCESS_JWKS_URL";

// ---- internal constants -----------------------------------------------------

/// The header Cloudflare Access injects on proxied requests.
pub(crate) const JWT_HEADER: &str = "cf-access-jwt-assertion";
/// The cookie Cloudflare Access sets for browser sessions.
pub(crate) const JWT_COOKIE_NAME: &str = "CF_Authorization";

const CACHE_TTL: Duration = Duration::from_secs(3600);
const REFRESH_RATE_LIMIT: Duration = Duration::from_secs(60);

// ---- configuration ----------------------------------------------------------

/// Startup configuration for the access layer.
pub struct AccessConfig {
    /// Full team URL, e.g. `https://myteam.cloudflareaccess.com`.
    pub team_domain: String,
    /// The Access application's audience tag.
    pub aud: String,
    /// The JWKS endpoint. Explicit rather than derived from `team_domain` so
    /// that test infrastructure can point at localhost without lying about `iss`.
    pub jwks_url: String,
}

impl AccessConfig {
    /// Read the three required env vars. Fails if any is absent.
    pub fn from_env() -> anyhow::Result<Self> {
        let team_domain = std::env::var(TEAM_DOMAIN_ENV)
            .map_err(|_| anyhow::anyhow!("{TEAM_DOMAIN_ENV} is required"))?;
        let aud = std::env::var(AUD_ENV).map_err(|_| anyhow::anyhow!("{AUD_ENV} is required"))?;
        let jwks_url = std::env::var(JWKS_URL_ENV)
            .map_err(|_| anyhow::anyhow!("{JWKS_URL_ENV} is required"))?;
        Ok(AccessConfig {
            team_domain,
            aud,
            jwks_url,
        })
    }

    /// Read the three env vars only when at least one is present in the
    /// environment. Returns `None` when none of the three are set — Access is
    /// not configured, and the server starts without the authentication layer.
    /// Returns `Err` when at least one is set but the set is incomplete: a
    /// partial configuration is a misconfiguration, not a quiet skip.
    ///
    /// This is the configuration-as-enablement idiom: setting up the config IS
    /// enabling the layer. There is no separate opt-in flag.
    pub fn from_env_if_configured() -> anyhow::Result<Option<Self>> {
        let any_set = [TEAM_DOMAIN_ENV, AUD_ENV, JWKS_URL_ENV]
            .iter()
            .any(|v| std::env::var(v).is_ok());
        if !any_set {
            return Ok(None);
        }
        Ok(Some(Self::from_env()?))
    }
}

// ---- JWKS cache -------------------------------------------------------------

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Jwk>,
}

struct CacheState {
    keys: HashMap<String, DecodingKey>,
    fetched_at: Instant,
    last_refresh_attempt: Option<Instant>,
}

struct JwksCache {
    inner: RwLock<Option<CacheState>>,
    jwks_url: String,
    client: reqwest::Client,
}

impl JwksCache {
    fn new(jwks_url: String, client: reqwest::Client) -> Self {
        JwksCache {
            inner: RwLock::new(None),
            jwks_url,
            client,
        }
    }

    async fn fetch(&self) -> anyhow::Result<HashMap<String, DecodingKey>> {
        let set: JwkSet = self
            .client
            .get(&self.jwks_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let mut keys = HashMap::new();
        for jwk in set.keys {
            if let Ok(k) = DecodingKey::from_rsa_components(&jwk.n, &jwk.e) {
                keys.insert(jwk.kid, k);
            }
        }
        Ok(keys)
    }

    /// Populate the cache. A failed fetch or an empty key set is fatal.
    async fn prime(&self) -> anyhow::Result<()> {
        let keys = self
            .fetch()
            .await
            .map_err(|e| anyhow::anyhow!("JWKS fetch from {} failed: {e}", self.jwks_url))?;
        if keys.is_empty() {
            anyhow::bail!("JWKS at {} returned no usable RSA keys", self.jwks_url);
        }
        *self.inner.write().await = Some(CacheState {
            keys,
            fetched_at: Instant::now(),
            last_refresh_attempt: None,
        });
        Ok(())
    }

    /// Return the decoding key for `kid`, refreshing when the cache is stale
    /// or the kid is unknown (subject to the rate limit).
    async fn get(&self, kid: &str) -> Result<DecodingKey, AppError> {
        // Fast path under a read lock.
        {
            let guard = self.inner.read().await;
            if let Some(c) = guard.as_ref()
                && c.fetched_at.elapsed() < CACHE_TTL
                && let Some(k) = c.keys.get(kid)
            {
                return Ok(k.clone());
            }
        }
        // Slow path: stale or unknown kid — acquire the write lock.
        let mut guard = self.inner.write().await;
        // Re-check: another task may have refreshed while we waited.
        if let Some(c) = guard.as_ref()
            && c.fetched_at.elapsed() < CACHE_TTL
        {
            if let Some(k) = c.keys.get(kid) {
                return Ok(k.clone());
            }
            // Unknown kid but within the rate-limit window.
            if c.last_refresh_attempt
                .map(|t| t.elapsed() < REFRESH_RATE_LIMIT)
                .unwrap_or(false)
            {
                return Err(AppError(
                    StatusCode::UNAUTHORIZED,
                    "unknown signing key".into(),
                ));
            }
        }
        let now = Instant::now();
        match self.fetch().await {
            Ok(keys) => {
                let found = keys.get(kid).cloned();
                *guard = Some(CacheState {
                    keys,
                    fetched_at: now,
                    last_refresh_attempt: Some(now),
                });
                found
                    .ok_or_else(|| AppError(StatusCode::UNAUTHORIZED, "unknown signing key".into()))
            }
            Err(_) => {
                if let Some(c) = guard.as_mut() {
                    c.last_refresh_attempt = Some(now);
                }
                Err(AppError(
                    StatusCode::UNAUTHORIZED,
                    "key refresh failed".into(),
                ))
            }
        }
    }
}

// ---- verified identity ------------------------------------------------------

/// Proof that the current request carried a valid Cloudflare Access JWT.
///
/// The constructor is private — the only source is [`layer`], which called
/// [`AccessState::verify`] and succeeded. A handler holding one is guaranteed
/// the request was authenticated by Cloudflare Access.
#[derive(Clone, Debug)]
pub struct VerifiedIdentity {
    /// User's email address, normalised to lowercase.
    email: String,
    /// The JWT `sub` claim — the Cloudflare Access user identity UUID.
    sub: String,
}

impl VerifiedIdentity {
    fn new(email: String, sub: String) -> Self {
        VerifiedIdentity {
            email: email.to_lowercase(),
            sub,
        }
    }

    pub fn email(&self) -> &str {
        &self.email
    }

    pub fn sub(&self) -> &str {
        &self.sub
    }
}

tokio::task_local! {
    static CURRENT: VerifiedIdentity;
}

/// The identity verified for the request being served on this task.
#[allow(dead_code)]
pub(crate) fn current() -> Result<VerifiedIdentity, AppError> {
    CURRENT.try_with(VerifiedIdentity::clone).map_err(|_| {
        AppError::internal(
            "no verified identity for this request — route is outside the access layer",
        )
    })
}

// ---- state and middleware ---------------------------------------------------

#[derive(Deserialize)]
struct Claims {
    email: String,
    sub: String,
}

/// Shared state for the access middleware.
pub struct AccessState {
    aud: String,
    iss: String,
    cache: Arc<JwksCache>,
}

impl AccessState {
    /// Build the access state and prime the JWKS cache.
    ///
    /// Fails if any env var is absent or if the initial JWKS fetch fails —
    /// both are startup failures.
    pub async fn new(cfg: AccessConfig) -> anyhow::Result<Arc<Self>> {
        let iss = cfg.team_domain.trim_end_matches('/').to_string();
        let client = reqwest::Client::new();
        let cache = Arc::new(JwksCache::new(cfg.jwks_url, client));
        cache.prime().await?;
        Ok(Arc::new(AccessState {
            aud: cfg.aud,
            iss,
            cache,
        }))
    }

    pub(crate) async fn verify(&self, token: &str) -> Result<VerifiedIdentity, AppError> {
        let header = jsonwebtoken::decode_header(token)
            .map_err(|_| AppError(StatusCode::UNAUTHORIZED, "invalid JWT".into()))?;
        let kid = header
            .kid
            .ok_or_else(|| AppError(StatusCode::UNAUTHORIZED, "JWT missing kid".into()))?;
        let key = self.cache.get(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_audience(&[&self.aud]);
        validation.set_issuer(&[&self.iss]);
        // jsonwebtoken v9 defaults to validate_nbf = false; enforce it explicitly.
        validation.validate_nbf = true;
        let data = decode::<Claims>(token, &key, &validation)
            .map_err(|e| AppError(StatusCode::UNAUTHORIZED, format!("JWT rejected: {e}")))?;
        Ok(VerifiedIdentity::new(data.claims.email, data.claims.sub))
    }
}

/// Extract the JWT from the `Cf-Access-Jwt-Assertion` header, falling back
/// to the `CF_Authorization` cookie.
fn extract_token(req: &axum::http::request::Parts) -> Option<String> {
    if let Some(v) = req.headers.get(JWT_HEADER)
        && let Ok(s) = v.to_str()
    {
        return Some(s.to_string());
    }
    for cookie_hdr in req.headers.get_all(header::COOKIE) {
        if let Ok(s) = cookie_hdr.to_str() {
            for part in s.split(';') {
                let t = part.trim();
                if let Some(v) = t
                    .strip_prefix(JWT_COOKIE_NAME)
                    .and_then(|s| s.strip_prefix('='))
                {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Middleware: verify the Cloudflare Access JWT, then run the rest of the
/// request with the identity in scope. Applied to `/api` as a `route_layer`.
///
/// When `state.access` is `None` (Access not configured), the request passes
/// through without authentication. This is the opt-in path: configuring the
/// three Access env vars enables the layer; leaving them unset disables it.
/// When the layer IS active, it is completely fail-closed — no token means 401.
pub(crate) async fn layer(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let Some(access) = state.access.as_ref() else {
        return Ok(next.run(request).await);
    };
    let (parts, body) = request.into_parts();
    let token = extract_token(&parts).ok_or_else(|| {
        AppError(
            StatusCode::UNAUTHORIZED,
            "missing Cloudflare Access token".into(),
        )
    })?;
    let identity = access.verify(&token).await?;
    let request = Request::from_parts(parts, body);
    Ok(CURRENT.scope(identity, next.run(request)).await)
}

// ---- test support -----------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::traits::PublicKeyParts;

    struct TestKey {
        kid: String,
        encoding_key: EncodingKey,
        n: String,
        e: String,
    }

    impl TestKey {
        fn generate(kid: impl Into<String>) -> Self {
            let mut rng = rsa::rand_core::OsRng;
            let sk = RsaPrivateKey::new(&mut rng, 2048).unwrap();
            let pk = sk.to_public_key();
            let n = URL_SAFE_NO_PAD.encode(pk.n().to_bytes_be());
            let e = URL_SAFE_NO_PAD.encode(pk.e().to_bytes_be());
            let pem = sk.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).unwrap();
            let encoding_key = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
            TestKey {
                kid: kid.into(),
                encoding_key,
                n,
                e,
            }
        }
    }

    /// An in-process JWKS server + RSA signing key for use in unit and
    /// integration tests.
    ///
    /// `TestAccessFixture::new()` spawns a tiny Axum server on a random
    /// loopback port, calls `AccessState::new` against it, and returns the
    /// fixture. The server lives for the duration of the Tokio runtime that
    /// created it — i.e. for the lifetime of the test.
    pub(crate) struct TestAccessFixture {
        pub(crate) access: Arc<AccessState>,
        pub(crate) aud: String,
        pub(crate) team_domain: String,
        key: TestKey,
    }

    impl TestAccessFixture {
        pub(crate) async fn new() -> Self {
            let key = TestKey::generate("test-kid-1");
            let aud = "test-aud-pkdump".to_string();
            let team_domain = "https://test.cloudflareaccess.com".to_string();

            // Build a JWKS document from the test key.
            let jwks = serde_json::json!({
                "keys": [{
                    "kty": "RSA",
                    "kid": key.kid,
                    "n": key.n,
                    "e": key.e,
                }]
            });
            let jwks_body = jwks.to_string();

            // Spin up a one-shot JWKS server on a random port.
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move {
                let body = jwks_body.clone();
                let app = axum::Router::new().route(
                    "/cdn-cgi/access/certs",
                    axum::routing::get(move || {
                        let b = body.clone();
                        async move {
                            axum::response::Response::builder()
                                .header("content-type", "application/json")
                                .body(axum::body::Body::from(b))
                                .unwrap()
                        }
                    }),
                );
                axum::serve(listener, app).await.ok();
            });

            let cfg = AccessConfig {
                team_domain: team_domain.clone(),
                aud: aud.clone(),
                jwks_url: format!("http://127.0.0.1:{port}/cdn-cgi/access/certs"),
            };
            let access = AccessState::new(cfg).await.unwrap();

            TestAccessFixture {
                access,
                aud,
                team_domain,
                key,
            }
        }

        /// Mint a valid JWT for `email`.
        pub(crate) fn valid_token(&self, email: &str) -> String {
            self.mint(email, &self.team_domain, &self.aud, 3600, -1)
        }

        /// A token signed with an unregistered key (unknown kid).
        pub(crate) fn unknown_kid_token(&self, email: &str) -> String {
            let throwaway = TestKey::generate("unknown-kid-xyz");
            let now = unix_now() as i64;
            let claims = serde_json::json!({
                "email": email,
                "sub": email,
                "iss": self.team_domain,
                "aud": [self.aud],
                "exp": now + 3600,
                "nbf": now - 1,
                "iat": now,
            });
            let mut hdr = Header::new(jsonwebtoken::Algorithm::RS256);
            hdr.kid = Some("unknown-kid-xyz".to_string());
            encode(&hdr, &claims, &throwaway.encoding_key).unwrap()
        }

        /// A token that is already expired.
        pub(crate) fn expired_token(&self, email: &str) -> String {
            self.mint(email, &self.team_domain, &self.aud, -3600, -7200)
        }

        /// A token whose `nbf` is in the future.
        pub(crate) fn not_yet_valid_token(&self, email: &str) -> String {
            self.mint(email, &self.team_domain, &self.aud, 7200, 3600)
        }

        /// A token with the wrong audience.
        pub(crate) fn wrong_aud_token(&self, email: &str) -> String {
            self.mint(email, &self.team_domain, "wrong-audience-xyz", 3600, -1)
        }

        /// A token with the wrong issuer.
        pub(crate) fn wrong_issuer_token(&self, email: &str) -> String {
            self.mint(
                email,
                "https://evil.cloudflareaccess.com",
                &self.aud,
                3600,
                -1,
            )
        }

        fn mint(
            &self,
            email: &str,
            iss: &str,
            aud: &str,
            exp_offset: i64,
            nbf_offset: i64,
        ) -> String {
            let now = unix_now() as i64;
            let claims = serde_json::json!({
                "email": email,
                "sub": email,
                "iss": iss,
                "aud": [aud],
                "exp": now + exp_offset,
                "nbf": now + nbf_offset,
                "iat": now,
            });
            let mut hdr = Header::new(jsonwebtoken::Algorithm::RS256);
            hdr.kid = Some(self.key.kid.clone());
            encode(&hdr, &claims, &self.key.encoding_key).unwrap()
        }

        /// A token with the payload bytes modified after signing (signature mismatch).
        pub(crate) fn tampered_token(&self, email: &str) -> String {
            let valid = self.valid_token(email);
            let mut parts: Vec<String> = valid.split('.').map(String::from).collect();
            let payload = &mut parts[1];
            // Flip the first character to invalidate the signature without
            // making the base64 unparseable.
            let first = payload.chars().next().unwrap();
            let replacement = if first == 'a' { 'b' } else { 'a' };
            *payload = format!("{replacement}{}", &payload[1..]);
            parts.join(".")
        }

        /// A manually constructed JWT with `"alg":"none"` — no signature.
        pub(crate) fn alg_none_token(&self, email: &str) -> String {
            let header_json = format!(r#"{{"alg":"none","typ":"JWT","kid":"{}"}}"#, self.key.kid);
            let header = URL_SAFE_NO_PAD.encode(header_json.as_bytes());
            let now = unix_now() as i64;
            let payload = serde_json::json!({
                "email": email,
                "sub": email,
                "iss": self.team_domain,
                "aud": [self.aud],
                "exp": now + 3600,
                "nbf": now - 1,
                "iat": now,
            });
            let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
            format!("{header}.{payload_b64}.")
        }

        /// A token signed with HS256 using the RSA modulus bytes as the HMAC
        /// secret — the classic algorithm-confusion attack.
        pub(crate) fn alg_hs256_pubkey_token(&self, email: &str) -> String {
            let n_bytes = URL_SAFE_NO_PAD.decode(&self.key.n).unwrap();
            let now = unix_now() as i64;
            let claims = serde_json::json!({
                "email": email,
                "sub": email,
                "iss": self.team_domain,
                "aud": [self.aud],
                "exp": now + 3600,
                "nbf": now - 1,
                "iat": now,
            });
            let mut hdr = Header::new(jsonwebtoken::Algorithm::HS256);
            hdr.kid = Some(self.key.kid.clone());
            encode(&hdr, &claims, &EncodingKey::from_secret(&n_bytes)).unwrap()
        }
    }

    fn unix_now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// Mint a `VerifiedIdentity` directly without going through JWT verification.
    /// Only for use in tests.
    pub(crate) fn identity(email: &str) -> super::VerifiedIdentity {
        super::VerifiedIdentity::new(email.to_lowercase(), email.to_string())
    }
}

// ---- unit tests -------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::test_support::TestAccessFixture;
    use super::*;
    use axum::http::Request as HttpRequest;

    // The eight required failure cases, each observed red (before this module
    // existed) and now green. They test `AccessState::verify` directly so the
    // assertions are tight: the right 401, not some other error.

    #[tokio::test]
    async fn valid_token_is_accepted() {
        let fx = TestAccessFixture::new().await;
        let token = fx.valid_token("Alice@Example.COM");
        let id = fx.access.verify(&token).await.unwrap();
        // Email is normalised to lowercase; sub is passed through unchanged.
        assert_eq!(id.email(), "alice@example.com");
        assert_eq!(id.sub(), "Alice@Example.COM");
    }

    #[tokio::test]
    async fn no_token_returns_none_from_extractor() {
        let (parts, _) = HttpRequest::builder()
            .uri("/api/test")
            .body(())
            .unwrap()
            .into_parts();
        assert!(
            extract_token(&parts).is_none(),
            "a request with no header and no cookie must produce None"
        );
    }

    #[tokio::test]
    async fn token_is_extracted_from_header() {
        let (parts, _) = HttpRequest::builder()
            .uri("/api/test")
            .header(JWT_HEADER, "header-token")
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(extract_token(&parts).as_deref(), Some("header-token"));
    }

    #[tokio::test]
    async fn token_is_extracted_from_cookie_when_no_header() {
        let cookie = format!("{JWT_COOKIE_NAME}=cookie-token; Path=/");
        let (parts, _) = HttpRequest::builder()
            .uri("/api/test")
            .header("cookie", cookie)
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(extract_token(&parts).as_deref(), Some("cookie-token"));
    }

    #[tokio::test]
    async fn header_takes_precedence_over_cookie() {
        let cookie = format!("{JWT_COOKIE_NAME}=cookie-token");
        let (parts, _) = HttpRequest::builder()
            .uri("/api/test")
            .header(JWT_HEADER, "header-token")
            .header("cookie", cookie)
            .body(())
            .unwrap()
            .into_parts();
        assert_eq!(extract_token(&parts).as_deref(), Some("header-token"));
    }

    #[tokio::test]
    async fn garbage_token_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let err = fx.access.verify("not.a.valid.jwt").await.unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED, "garbage token must be 401");
    }

    #[tokio::test]
    async fn expired_token_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.expired_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED, "expired token must be 401");
    }

    #[tokio::test]
    async fn not_yet_valid_token_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.not_yet_valid_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::UNAUTHORIZED,
            "nbf-future token must be 401"
        );
    }

    #[tokio::test]
    async fn wrong_audience_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.wrong_aud_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::UNAUTHORIZED,
            "wrong-aud token must be 401"
        );
    }

    #[tokio::test]
    async fn wrong_issuer_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.wrong_issuer_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::UNAUTHORIZED,
            "wrong-issuer token must be 401"
        );
    }

    #[tokio::test]
    async fn unknown_kid_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.unknown_kid_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::UNAUTHORIZED,
            "unknown-kid token must be 401"
        );
    }

    #[tokio::test]
    async fn tampered_payload_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.tampered_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::UNAUTHORIZED,
            "a token with a tampered payload must be 401"
        );
    }

    #[tokio::test]
    async fn alg_none_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.alg_none_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::UNAUTHORIZED,
            "alg:none token must be 401 (classic JWT defeat)"
        );
    }

    #[tokio::test]
    async fn alg_hs256_signed_with_public_key_is_rejected() {
        let fx = TestAccessFixture::new().await;
        let token = fx.alg_hs256_pubkey_token("alice@example.com");
        let err = fx.access.verify(&token).await.unwrap_err();
        assert_eq!(
            err.0,
            StatusCode::UNAUTHORIZED,
            "HS256-signed-with-public-key token must be 401 (algorithm confusion)"
        );
    }
}
