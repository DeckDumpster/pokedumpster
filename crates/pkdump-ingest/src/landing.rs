//! The one place an upstream response turns into bytes.
//!
//! Every client in this crate that we keep bytes from fetches through
//! [`fetch_bytes`], so "land what we fetched" is a property of a single
//! function rather than a discipline each client has to remember. Before
//! this existed both clients called `.json()` on the response and the raw
//! bytes were never visible at all.
//!
//! Because it is one function, it is also the only place the *inverse* has
//! to be expressed: a derive that replays bytes already landed instead of
//! fetching them. A [`Wire`] carries both halves —
//!
//! - **neither** — fetch, hand the bytes back, parse. Exactly the behaviour
//!   this crate had before the landing zone existed, which is what every
//!   offline gate and container test still runs.
//! - **a landing zone** — the same fetch, and the same bytes additionally
//!   stored before anything looks at them.
//! - **a replay source** — no fetch at all. The bytes come from `raw/`, and
//!   the code above this function cannot tell the difference, which is the
//!   point: the derivation that decides what a card *is* must be the same
//!   code either way, or "row-identical" would be proving something about a
//!   second implementation.
//!
//! Being one function is also what makes a **bounded retry** one thing rather
//! than a discipline. Every request every client makes is executed here, so
//! [`crate::retry`]'s budget applies to all of them by construction; a client
//! added later cannot forget to retry, because it does not do the fetching.
//!
//! What deliberately does **not** live here is the policy for a replay MISS.
//! A URL the landing zone has no record of means raw coverage has regressed,
//! and how loudly to say so — and whether to reach upstream anyway — is the
//! offline job's decision, not this crate's. See [`ReplaySource::missing`].

use std::sync::Arc;

use pkdump_lake::{Dataset, PartFormat, RawLanding, Source};

use crate::error::{IngestError, Result};
use crate::retry::{self, RetryPolicy};

/// A landing zone shared by every client of one invocation, or `None`.
pub type Landing = Option<Arc<RawLanding>>;

/// Bytes an upstream already returned, keyed by the URL that returned them.
///
/// Implemented by the offline derive over the raw landing zone. Nothing in
/// this crate constructs one, and nothing on the serving path can: reading
/// `raw/` is lakehouse work, and the whole reason this is a trait is to keep
/// that code on the other side of the eventual machine split.
pub trait ReplaySource: Send + Sync {
    /// The body this URL returned when it was landed, or `None` when the
    /// landing zone has no record of the URL at all.
    fn body(&self, url: &str) -> Result<Option<Vec<u8>>>;

    /// Called when [`body`](ReplaySource::body) returned `None` and the fetch
    /// is about to reach the real upstream instead.
    ///
    /// This is the fallback's one seam. Returning `Ok(())` lets the live
    /// fetch happen — the implementation is expected to have *said so
    /// loudly*, because a URL missing from `raw/` means the landing zone no
    /// longer covers the derivation's inputs. Returning `Err` refuses
    /// instead, which is what the derive does when the fallback is switched
    /// off (and what it will do unconditionally once item 4 removes it).
    fn missing(&self, url: &str) -> Result<()>;
}

/// How a client's requests are answered: where the bytes come from, how hard
/// it tries to get them, and what is kept.
///
/// Cloneable and cheap: every client of one invocation holds the same two
/// `Arc`s. A default `Wire` neither lands nor replays — the client that
/// existed before any of this, now with [`crate::retry`]'s budget behind it.
#[derive(Clone, Default)]
pub struct Wire {
    landing: Landing,
    replay: Option<Arc<dyn ReplaySource>>,
    /// Defaults through [`RetryPolicy::default`], which reads the
    /// environment — so a `Wire` nobody configured still retries, and a unit
    /// file can widen the budget without a rebuild.
    retry: RetryPolicy,
}

impl Wire {
    /// Land every response in `landing`, on top of whatever else is set.
    pub fn landing_in(mut self, landing: Arc<RawLanding>) -> Self {
        self.landing = Some(landing);
        self
    }

    /// Serve responses from `replay` instead of fetching them.
    pub fn replaying(mut self, replay: Arc<dyn ReplaySource>) -> Self {
        self.replay = Some(replay);
        self
    }

    /// Use `retry` instead of the environment's budget.
    ///
    /// The reason this is on the `Wire` and not on each client: one
    /// invocation builds one `Wire` and hands it to every client it
    /// constructs — including the one `japan::import_all` builds internally,
    /// which nothing outside that function can otherwise reach.
    pub fn retrying(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Whether requests are answered from `raw/` rather than the network.
    ///
    /// Clients read this to skip their inter-request politeness sleep: a
    /// derive that slept 50ms per replayed part would spend most of its
    /// runtime being polite to a server it is not talking to.
    pub fn is_replaying(&self) -> bool {
        self.replay.is_some()
    }
}

/// One attempt that got the whole body back.
struct Fetched {
    status: u16,
    body: Vec<u8>,
}

/// One attempt that did not.
struct Failed {
    /// The HTTP status, when the request got far enough to have one.
    status: Option<u16>,
    /// Whether asking again could plausibly answer differently — see
    /// [`crate::retry`].
    retryable: bool,
    /// The text the manifest records. Kept separately from `error` so it is
    /// the upstream's own words rather than this crate's wrapping of them.
    text: String,
    error: IngestError,
}

/// Send one request and read its body.
fn attempt(
    http: &reqwest::blocking::Client,
    request: reqwest::blocking::Request,
) -> std::result::Result<Fetched, Failed> {
    let fail = |status: Option<u16>, retryable: bool, e: reqwest::Error| Failed {
        status,
        retryable,
        text: e.to_string(),
        error: e.into(),
    };

    // Connect, TLS, timeout: nothing about the request was rejected, so
    // nothing about the request needs changing before trying again.
    let response = http.execute(request).map_err(|e| fail(None, true, e))?;

    let status = response.status().as_u16();
    let response = response
        .error_for_status()
        .map_err(|e| fail(Some(status), retry::status_is_retryable(status), e))?;

    // A body that stopped arriving is a transport failure like any other; the
    // status only says the upstream started answering.
    let body = response.bytes().map_err(|e| fail(Some(status), true, e))?;
    Ok(Fetched {
        status,
        body: body.to_vec(),
    })
}

/// Execute `req` and return the response body, landing it on the way past.
///
/// With a replay source on the `wire` the request is answered from `raw/`
/// and never sent. A URL the landing zone does not hold goes through
/// [`ReplaySource::missing`] first, which either refuses or lets the live
/// fetch below proceed.
///
/// A transport failure, a 429 or a 5xx is retried on the `wire`'s budget —
/// see [`crate::retry`], which is also where the argument for why that is not
/// fallback logic lives. Every other non-2xx is answered once.
///
/// The failure that finally propagates — the last attempt's — is recorded in
/// the run's manifest before it does, so a run that dies partway leaves
/// evidence of where it stopped rather than a short prefix that reads as
/// whole. **Only that one is recorded.** A manifest failure means "this URL
/// was not fetched", and `complete` is computed from `failures.is_empty()`
/// (`pkdump_lake::sink::finalize`), so logging the attempts a retry went on
/// to recover from would mark a whole run incomplete for a hiccup it
/// survived. The retries are still visible — loudly, on stderr, in the
/// journal the nightly unit writes.
///
/// The fetch error is what propagates; a manifest that could not be written
/// warns rather than masking it.
pub fn fetch_bytes(
    http: &reqwest::blocking::Client,
    req: reqwest::blocking::RequestBuilder,
    wire: &Wire,
    source: Source,
    dataset: Dataset,
    format: PartFormat,
) -> Result<Vec<u8>> {
    // Build first so the manifest records the URL actually requested, query
    // string and all, rather than a reconstruction of it. It is also the key
    // a replay looks up, which is only sound because both sides build it the
    // same way — here.
    let request = req.build()?;
    let url = request.url().to_string();

    if let Some(replay) = &wire.replay {
        match replay.body(&url)? {
            Some(body) => return Ok(body),
            // Not in raw. The implementation decides whether that is fatal;
            // if it is not, fall through and fetch it for real.
            None => replay.missing(&url)?,
        }
    }

    let landing = wire.landing.as_ref();
    let note = |status: Option<u16>, error: &str| {
        if let Some(landing) = landing
            && let Err(e) = landing.record_failure(source, dataset, &url, status, error)
        {
            eprintln!("WARN: could not record {source}/{dataset} fetch failure in the lake: {e}");
        }
    };

    let mut n = 1;
    let fetched = loop {
        // Each attempt sends its own copy — `execute` consumes the request.
        // A body that cannot be replayed cannot be retried either; no client
        // in this crate sends one, so this refuses rather than quietly
        // falling back to a single attempt.
        let Some(this) = request.try_clone() else {
            return Err(IngestError::BadResponse(format!(
                "{url}: request body cannot be replayed, so the fetch cannot be retried"
            )));
        };
        match attempt(http, this) {
            Ok(fetched) => break fetched,
            Err(failed) => {
                if !(failed.retryable && wire.retry.should_retry(n)) {
                    note(failed.status, &failed.text);
                    return Err(failed.error);
                }
                let delay = wire.retry.delay_after(n);
                eprintln!(
                    "!! {source}/{dataset} attempt {n}/{} failed ({}): {} — retrying in {}ms",
                    wire.retry.attempts,
                    failed.text,
                    url,
                    delay.as_millis()
                );
                std::thread::sleep(delay);
                n += 1;
            }
        }
    };

    if n > 1 {
        eprintln!("!! {source}/{dataset} recovered on attempt {n}: {url}");
    }
    if let Some(landing) = landing {
        landing.land(source, dataset, &url, fetched.status, format, &fetched.body)?;
    }
    Ok(fetched.body)
}
