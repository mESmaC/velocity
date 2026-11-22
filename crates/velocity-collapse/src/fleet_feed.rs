//! Sends per-hour fleet rollup counts and durations to Collapse's project state feed
//! (`<collapse_root>/rosen/state`, #619, mESmaC/collapse.sh#225).
//!
//! Only numeric counts and durations cross the wire — never issue titles, URLs, prompts,
//! agent transcripts or file paths. Uses the same per-path push token Rosen uses for the
//! state feed (`username = "rosen"`, password from git's credential helper).

use std::time::Duration;

use reqwest::Client;
use serde::Serialize;

use crate::CollapseError;

const PUSH_TIMEOUT: Duration = Duration::from_secs(15);
const STATE_FEED_USERNAME: &str = "rosen";
const STATE_FEED_FORMAT: u32 = 1;

/// Counts-and-durations-only hourly fleet rollup item sent on Collapse's state feed (#619).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct FleetRollupFeedItem {
    pub hour_start: u64,
    pub lands: u32,
    pub issues_dispatched: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_filed_to_landed_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_dispatched_to_landed_secs: Option<u64>,
    pub idle_slot_seconds: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_landed_change: Option<u64>,
    pub post_land_failures: u32,
    pub reverts: u32,
    pub checks_queued: u32,
    pub check_queue_wait_ms_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_queue_wait_ms_median: Option<u64>,
    pub check_run_ms_total: u64,
    pub retries: u32,
    pub review_findings: u32,
}

#[derive(Serialize)]
struct StateFeedFleetBatch<'a> {
    format: u32,
    through: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    feed_id: Option<&'a str>,
    fleet_rollups: &'a [FleetRollupFeedItem],
}

/// Posts `rollups` (counts and durations only) to `<collapse_root>/rosen/state`.
/// Returns `Ok(false)` when no push token is stored for the endpoint or `rollups` is empty.
pub async fn push_fleet_rollups(
    collapse_root: &str,
    feed_id: Option<&str>,
    rollups: &[FleetRollupFeedItem],
) -> Result<bool, CollapseError> {
    if rollups.is_empty() {
        return Ok(false);
    }
    let endpoint = format!("{}/rosen/state", collapse_root.trim_end_matches('/'));
    let lookup_url = endpoint.clone();
    let secret =
        tokio::task::spawn_blocking(move || crate::git_credential::load_push_secret(&lookup_url))
            .await
            .map_err(|error| CollapseError::Git(error.to_string()))??;
    let Some(secret) = secret else {
        return Ok(false);
    };
    let batch = StateFeedFleetBatch {
        format: STATE_FEED_FORMAT,
        through: 0,
        feed_id: feed_id.map(str::trim).filter(|id| !id.is_empty()),
        fleet_rollups: rollups,
    };
    let client = Client::builder()
        .timeout(PUSH_TIMEOUT)
        .build()
        .map_err(|error| CollapseError::Api(error.to_string()))?;
    let response = client
        .post(&endpoint)
        .basic_auth(STATE_FEED_USERNAME, Some(secret))
        .json(&batch)
        .send()
        .await
        .map_err(|error| CollapseError::Api(format!("Fleet rollup push failed: {error}")))?;
    let status = response.status();
    if status.is_success() {
        return Ok(true);
    }
    let detail = response.text().await.unwrap_or_default();
    let detail = detail.trim();
    Err(CollapseError::Api(if detail.is_empty() {
        format!("Collapse state feed returned HTTP {}", status.as_u16())
    } else {
        format!(
            "Collapse state feed returned HTTP {}: {detail}",
            status.as_u16()
        )
    }))
}
