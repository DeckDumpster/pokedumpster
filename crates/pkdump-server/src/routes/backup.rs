//! `/api/backup-status` — Layer 3 of the backup alarming (pokedumpster-ivq.5):
//! passive in-app visibility of off-box backup freshness.
//!
//! The host-side Layer 1 checker (`deploy/backup-check.sh`) writes a
//! `.backup-last-ok` marker (a Unix epoch) onto the data volume each time it
//! confirms the Litestream S3 replica is fresh. The server reads only that
//! marker — it needs no S3 credentials of its own. The frontend renders a
//! staleness banner when a marker exists but has gone old, so a regressed
//! backup is obvious the moment you open the app over WireGuard.
//!
//! A MISSING marker is reported as "unknown" (not stale): on dev/test boxes and
//! before Layer 1 is armed there is no marker, and the off-box monitor — not
//! this banner — is what catches a never-configured backup. The banner only
//! flags a backup that *was* working and went stale.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::{AppError, AppState};

/// Default staleness threshold (hours) when `PKDUMP_BACKUP_STALE_HOURS` is
/// unset. Comfortably above the checker's 6h cadence so a single missed run
/// doesn't flip the banner.
const DEFAULT_STALE_HOURS: i64 = 12;

/// Off-box backup freshness, derived from the `.backup-last-ok` marker.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BackupStatus {
    /// Unix epoch (seconds) of the last confirmed-fresh backup check, or
    /// `null` when no marker exists yet (Layer 1 unarmed / never succeeded).
    pub last_ok_epoch: Option<i64>,
    /// Age of that confirmation in seconds, or `null` when unknown.
    pub age_seconds: Option<i64>,
    /// True only when a marker exists AND is older than the threshold.
    pub stale: bool,
    /// The staleness threshold in seconds, for display.
    pub stale_threshold_seconds: i64,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/backup-status", get(status))
}

async fn status(State(state): State<AppState>) -> Result<Json<BackupStatus>, AppError> {
    let threshold_hours = std::env::var("PKDUMP_BACKUP_STALE_HOURS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(DEFAULT_STALE_HOURS);
    let stale_threshold_seconds = threshold_hours * 3600;

    let marker = state.data_dir.join(".backup-last-ok");
    let last_ok_epoch = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok());

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| AppError::internal(format!("clock before epoch: {e}")))?
        .as_secs() as i64;

    let age_seconds = last_ok_epoch.map(|t| now - t);
    let stale = age_seconds.is_some_and(|age| age > stale_threshold_seconds);

    Ok(Json(BackupStatus {
        last_ok_epoch,
        age_seconds,
        stale,
        stale_threshold_seconds,
    }))
}
