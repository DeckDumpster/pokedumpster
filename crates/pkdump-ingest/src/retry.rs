//! How hard a fetch tries before it gives up.
//!
//! On 2026-08-11 `api.pokemontcg.io` answered 500 or 502 to roughly 45% of
//! requests for most of a day. Neither client retried anything, so a single
//! 5xx on `/v2/sets?page=1` ended `pkdump data refresh` in its first second —
//! and because the pokemontcg.io tail ran *before* TCGCSV, no prices were
//! imported at all. A day's prices cannot be re-fetched later, so each of
//! those nights was lost outright (pd-nons).
//!
//! ## This is not fallback logic
//!
//! The project's rule is that errors propagate: no silent defaults, no
//! swallowed exceptions. A bounded retry does not break it. Nothing is
//! defaulted, nothing is substituted, and no result is invented — the request
//! is *made again*, and when the budget is spent the original failure
//! propagates exactly as it does today. What changes is how many transport
//! hiccups it takes to lose a night, not what a failure means.
//!
//! ## What is retried
//!
//! Only failures that say nothing about the request:
//!
//! - **transport** — connect, TLS, timeout, a connection dropped mid-body.
//! - **429** and **5xx** — the upstream saying "not now", not "not this".
//!
//! Every other non-2xx is answered once. A 404 is a fact about the URL and
//! asking again is just noise; a 401 will be a 401 next time too.
//!
//! ## The schedule
//!
//! Exponential, from [`RetryPolicy::base_delay`], doubling per attempt and
//! capped at [`RetryPolicy::max_delay`]: 500ms, 1s, 2s by default, so four
//! attempts spend at most 3.5s of sleep on a URL that never answers.
//!
//! No jitter, deliberately. Jitter exists to break up a herd of clients
//! retrying in lockstep; this is one blocking client issuing one request at a
//! time, and a schedule a test can predict is worth more here than a spread
//! that has nothing to spread.

use std::time::Duration;

/// Total attempts per URL, first one included. `1` disables retrying.
pub const DEFAULT_ATTEMPTS: u32 = 4;

/// The first backoff. Doubles per attempt.
pub const DEFAULT_BASE_DELAY: Duration = Duration::from_millis(500);

/// The ceiling the doubling saturates at.
pub const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(8);

/// Overrides [`DEFAULT_ATTEMPTS`]. See [`RetryPolicy::from_env`].
pub const ENV_ATTEMPTS: &str = "PKDUMP_HTTP_RETRY_ATTEMPTS";

/// Overrides [`DEFAULT_BASE_DELAY`], in milliseconds. See
/// [`RetryPolicy::from_env`].
pub const ENV_BASE_DELAY_MS: &str = "PKDUMP_HTTP_RETRY_BASE_MS";

/// How many times a fetch is attempted, and how long it waits between tries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per URL, the first included. Never zero — a policy that
    /// makes no request at all is not a policy, so [`RetryPolicy::new`]
    /// floors it at 1.
    pub attempts: u32,
    /// The delay before attempt 2. Doubles for each attempt after that.
    pub base_delay: Duration,
    /// The longest the doubling is allowed to reach.
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// A policy with `attempts` tries, `base_delay` before the second, and
    /// [`DEFAULT_MAX_DELAY`] as the ceiling.
    pub fn new(attempts: u32, base_delay: Duration) -> Self {
        Self {
            attempts: attempts.max(1),
            base_delay,
            max_delay: DEFAULT_MAX_DELAY,
        }
    }

    /// One attempt, no waiting. What the clients did before pd-nons — kept
    /// as a named thing so a test that wants the old behaviour says so.
    pub fn none() -> Self {
        Self::new(1, Duration::ZERO)
    }

    /// The defaults, unless the environment names something else.
    ///
    /// The nightly refresh runs inside a container from a systemd unit, so
    /// the retry budget is reachable there without a rebuild — a week of bad
    /// upstream weather can be ridden out by editing a unit rather than
    /// shipping an image. It is also how the container gate makes retries
    /// fast enough to assert on.
    ///
    /// A value that is not a number, or is empty, is not a budget: it is
    /// ignored and the default stands. Erring the other way — refusing to
    /// start — would let a typo in an environment file cost the same night
    /// this module exists to save.
    pub fn from_env() -> Self {
        let attempts = env_u32(ENV_ATTEMPTS).unwrap_or(DEFAULT_ATTEMPTS);
        let base = env_u32(ENV_BASE_DELAY_MS)
            .map(|ms| Duration::from_millis(u64::from(ms)))
            .unwrap_or(DEFAULT_BASE_DELAY);
        let policy = Self::new(attempts, base);
        if policy != Self::new(DEFAULT_ATTEMPTS, DEFAULT_BASE_DELAY) {
            eprintln!(
                "!! HTTP retry budget overridden: {} attempt(s), {}ms base backoff",
                policy.attempts,
                policy.base_delay.as_millis()
            );
        }
        policy
    }

    /// How long to wait before attempt `attempt + 1`, given `attempt`
    /// (1-based) has just failed.
    pub fn delay_after(&self, attempt: u32) -> Duration {
        let shift = attempt.saturating_sub(1).min(16);
        let scaled = self.base_delay.saturating_mul(1u32 << shift);
        scaled.min(self.max_delay)
    }

    /// Whether another attempt is left after `attempt` (1-based) failed.
    pub fn should_retry(&self, attempt: u32) -> bool {
        attempt < self.attempts
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Whether an HTTP status is worth asking again.
///
/// 429 and 5xx are the upstream saying "not now". Everything else it answers
/// is a fact about the request, and a second identical request gets the same
/// fact back.
pub fn status_is_retryable(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schedule_doubles_and_then_saturates() {
        let p = RetryPolicy::new(6, Duration::from_millis(500));
        assert_eq!(p.delay_after(1), Duration::from_millis(500));
        assert_eq!(p.delay_after(2), Duration::from_secs(1));
        assert_eq!(p.delay_after(3), Duration::from_secs(2));
        assert_eq!(p.delay_after(4), Duration::from_secs(4));
        // Capped, not doubled forever.
        assert_eq!(p.delay_after(5), DEFAULT_MAX_DELAY);
        assert_eq!(p.delay_after(50), DEFAULT_MAX_DELAY);
    }

    #[test]
    fn the_budget_is_bounded() {
        let p = RetryPolicy::new(4, Duration::from_millis(1));
        assert!(p.should_retry(1));
        assert!(p.should_retry(3));
        assert!(!p.should_retry(4), "the fourth attempt is the last one");
    }

    /// A policy that never makes a request would turn every fetch into a
    /// failure with no upstream involved at all.
    #[test]
    fn zero_attempts_is_floored_to_one() {
        assert_eq!(RetryPolicy::new(0, Duration::ZERO).attempts, 1);
        assert!(!RetryPolicy::none().should_retry(1));
    }

    #[test]
    fn only_not_now_is_retried() {
        assert!(status_is_retryable(429));
        assert!(status_is_retryable(500));
        assert!(status_is_retryable(502));
        assert!(status_is_retryable(503));
        assert!(status_is_retryable(599));
        // Facts about the request, not the moment.
        assert!(!status_is_retryable(400));
        assert!(!status_is_retryable(401));
        assert!(!status_is_retryable(403));
        assert!(!status_is_retryable(404));
        assert!(!status_is_retryable(200));
    }
}
