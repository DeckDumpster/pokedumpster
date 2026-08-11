//! Where the upstream clients point, and the one way that can be changed.
//!
//! Every client in this crate has a `base_url()` builder already, and every
//! one of them is documented "test-tier only" — but a builder is only reachable
//! from a Rust test that constructs the client itself. `pkdump data refresh`
//! constructs its own, three of them, one of them inside `japan::import_all`,
//! so nothing outside this crate could point a whole refresh anywhere.
//!
//! That gap is why `tests/refresh/tenant_bytes.sh` could not exist. The claim
//! it makes — a catalog refresh writes the shared catalog and NOT one byte of
//! any tenant database — is only worth anything if the refresh *runs to the
//! end*, and a refresh that dies at the first fetch proves nothing. Reaching
//! the real tcgcsv.com to get there would make a gate that is slow, flaky, and
//! quietly dependent on somebody else's uptime.
//!
//! So the origin is read from the environment, in one place, by the two
//! constructors every client path goes through.
//!
//! **An override is announced on stderr, every time.** A catalog silently built
//! from somewhere other than upstream is a far worse failure than a noisy one:
//! the rows look ordinary and nothing downstream can tell. Unset — which is the
//! only way prod ever runs — nothing is printed and the URL is the constant it
//! always was.

/// `TcgcsvClient`'s origin. Test-tier; see the module docs.
pub const ENV_TCGCSV_BASE_URL: &str = "PKDUMP_TCGCSV_BASE_URL";

/// `PokemonTcgClient`'s origin. Test-tier; see the module docs.
pub const ENV_POKEMONTCG_BASE_URL: &str = "PKDUMP_POKEMONTCG_BASE_URL";

/// `default`, unless `env` names somewhere else to look.
///
/// A set-but-empty variable is not an origin, and taking it as one would build
/// every request against `/3/groups` with no host. It is treated as unset.
pub fn base_url(env: &str, default: &str) -> String {
    match std::env::var(env) {
        Ok(v) if !v.trim().is_empty() => {
            let v = v.trim().trim_end_matches('/').to_string();
            eprintln!("!! {env}={v} — upstream is NOT {default}");
            v
        }
        _ => default.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialised: these tests mutate one process-wide environment.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_var<T>(key: &str, value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: the lock above is the only thing touching the environment in
        // this crate's tests, and the variable is restored before it is dropped.
        unsafe {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
        let out = f();
        unsafe { std::env::remove_var(key) };
        out
    }

    const KEY: &str = "PKDUMP_TEST_BASE_URL_UNIT";

    #[test]
    fn unset_is_the_default() {
        assert_eq!(
            with_var(KEY, None, || base_url(KEY, "https://tcgcsv.com/tcgplayer")),
            "https://tcgcsv.com/tcgplayer"
        );
    }

    #[test]
    fn an_override_wins_and_loses_its_trailing_slash() {
        assert_eq!(
            with_var(KEY, Some("http://fake:8080/up/"), || base_url(
                KEY,
                "https://real"
            )),
            "http://fake:8080/up"
        );
    }

    /// Empty and whitespace are not origins. Taking one would build every
    /// request against a bare path and fail somewhere far from here.
    #[test]
    fn blank_is_treated_as_unset() {
        assert_eq!(
            with_var(KEY, Some(""), || base_url(KEY, "https://real")),
            "https://real"
        );
        assert_eq!(
            with_var(KEY, Some("   "), || base_url(KEY, "https://real")),
            "https://real"
        );
    }
}
