//! The one place an upstream response turns into bytes.
//!
//! Every client in this crate that we keep bytes from fetches through
//! [`fetch_bytes`], so "land what we fetched" is a property of a single
//! function rather than a discipline each client has to remember. Before
//! this existed both clients called `.json()` on the response and the raw
//! bytes were never visible at all.
//!
//! Landing is optional and off by default: with no sink the behaviour is
//! exactly what it was — fetch, hand the bytes back, parse. With a sink the
//! same bytes are additionally stored, before anything looks at them.

use std::sync::Arc;

use pkdump_lake::{Dataset, PartFormat, RawLanding, Source};

use crate::error::Result;

/// A landing zone shared by every client of one invocation, or `None`.
pub type Landing = Option<Arc<RawLanding>>;

/// Execute `req` and return the response body, landing it on the way past.
///
/// A failure — transport, non-2xx, or a truncated body — is recorded in the
/// run's manifest before it propagates, so a run that dies partway leaves
/// evidence of where it stopped rather than a short prefix that reads as
/// whole. The fetch error is what propagates; a manifest that could not be
/// written warns rather than masking it.
pub fn fetch_bytes(
    http: &reqwest::blocking::Client,
    req: reqwest::blocking::RequestBuilder,
    landing: Option<&Arc<RawLanding>>,
    source: Source,
    dataset: Dataset,
    format: PartFormat,
) -> Result<Vec<u8>> {
    // Build first so the manifest records the URL actually requested, query
    // string and all, rather than a reconstruction of it.
    let request = req.build()?;
    let url = request.url().to_string();

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
