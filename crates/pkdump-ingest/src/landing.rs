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
//! A replay MISS is a **refusal**, and it is one structurally: with a replay
//! source on the wire there is no path from here to the network at all. What
//! this crate does not decide is what the refusal SAYS — a URL the landing zone
//! has no record of means raw coverage has regressed, and diagnosing that is
//! the offline job's business, not this one's. See [`ReplaySource::missing`],
//! which returns the error rather than a licence to continue.

use std::sync::Arc;

use pkdump_lake::{Dataset, PartFormat, RawLanding, Source};

use crate::error::Result;

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

    /// The error for a URL [`body`](ReplaySource::body) had no record of.
    ///
    /// It returns the failure rather than a `Result`, because there is no
    /// success to report: a miss means the landing zone no longer covers the
    /// derivation's inputs, and the run stops. Item 2 of the epic had this
    /// return `Result<()>` so an implementation could answer `Ok(())` and let
    /// [`fetch_bytes`] reach the live upstream instead; item 4 removed that
    /// fallback, and removed the seam with it (pd-6yql). A miss can no longer
    /// reach the network by construction rather than by policy — reinstating a
    /// fallback would take changing this signature, which is the point.
    ///
    /// All the implementation supplies is the diagnosis, since only it knows
    /// which partition was being read.
    fn missing(&self, url: &str) -> crate::error::IngestError;
}

/// What a client does with the bytes it fetches, and where they come from.
///
/// Cloneable and cheap: every client of one invocation holds the same two
/// `Arc`s. An empty `Wire` is the client that existed before any of this.
#[derive(Clone, Default)]
pub struct Wire {
    landing: Landing,
    replay: Option<Arc<dyn ReplaySource>>,
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

    /// Whether requests are answered from `raw/` rather than the network.
    ///
    /// Clients read this to skip their inter-request politeness sleep: a
    /// derive that slept 50ms per replayed part would spend most of its
    /// runtime being polite to a server it is not talking to.
    pub fn is_replaying(&self) -> bool {
        self.replay.is_some()
    }
}

/// Execute `req` and return the response body, landing it on the way past.
///
/// With a replay source on the `wire` the request is answered from `raw/` and
/// never sent — including when the answer is that there is no record of it,
/// which is [`ReplaySource::missing`]'s error and the end of the run. Nothing
/// below this point runs on a replaying wire.
///
/// A failure — transport, non-2xx, or a truncated body — is recorded in the
/// run's manifest before it propagates, so a run that dies partway leaves
/// evidence of where it stopped rather than a short prefix that reads as
/// whole. The fetch error is what propagates; a manifest that could not be
/// written warns rather than masking it.
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
        return match replay.body(&url)? {
            Some(body) => Ok(body),
            // Not in raw, and there is nowhere else to look: a replaying wire
            // never reaches the network. The implementation supplies the
            // diagnosis; this is the end of the run either way.
            None => Err(replay.missing(&url)),
        };
    }

    let landing = wire.landing.as_ref();
    let note = |status: Option<u16>, error: &str| {
        if let Some(landing) = landing
            && let Err(e) = landing.record_failure(source, dataset, &url, status, error)
        {
            eprintln!("WARN: could not record {source}/{dataset} fetch failure in the lake: {e}");
        }
    };

    let response = match http.execute(request) {
        Ok(response) => response,
        Err(e) => {
            note(None, &e.to_string());
            return Err(e.into());
        }
    };

    let status = response.status().as_u16();
    let response = match response.error_for_status() {
        Ok(response) => response,
        Err(e) => {
            note(Some(status), &e.to_string());
            return Err(e.into());
        }
    };

    let body = match response.bytes() {
        Ok(body) => body,
        Err(e) => {
            note(Some(status), &e.to_string());
            return Err(e.into());
        }
    };

    if let Some(landing) = landing {
        landing.land(source, dataset, &url, status, format, &body)?;
    }
    Ok(body.to_vec())
}
