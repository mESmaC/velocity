//! Pure synchronous fleet development ledger and hourly rollup math (#619).
//!
//! Persists one record per dispatched issue (timings, provider/model, tokens, wall time,
//! turns, queued checks, restarts, retries, review findings) and hourly idle slot-seconds,
//! then rolls them up per project per UTC hour for the status pane, AIRC `fleet_stats`,
//! and Collapse's state feed (counts and durations only).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::improvement_outcomes::{
    FleetImprovementOutcomeView, ImprovementOutcomeRecord, build_recent_improvement_views,
};

/// One hour in seconds.
pub const HOUR_SECONDS: u64 = 3_600;
/// Retain up to 90 days of hourly idle slot-second buckets.
pub const MAX_IDLE_HOURS: usize = 90 * 24;
/// Retain up to 100 queued check records per issue to keep persisted state bounded.
pub const MAX_CHECKS_PER_ISSUE: usize = 100;
/// Retain up to 1,000 most recently updated issue records per project.
pub const MAX_ISSUES_PER_PROJECT: usize = 1_000;
/// Maximum offenders shown per category in status pane and `fleet_stats`.
pub const TOP_OFFENDERS_LIMIT: usize = 5;

/// Deterministic storage key for an issue record inside a project's fleet ledger.
pub fn issue_key(repository: &str, issue: u32) -> String {
    format!("{}#{issue}", repository.trim().to_ascii_lowercase())
}

/// Floors a UTC Unix-seconds timestamp to the start of its UTC hour.
pub const fn hour_start_utc(unix_seconds: u64) -> u64 {
    (unix_seconds / HOUR_SECONDS) * HOUR_SECONDS
}

/// One queued check (`queue_run` or `preflight`) recorded against an issue.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetCheckRecord {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub command: String,
    /// UTC Unix seconds when the check was queued.
    #[serde(default)]
    pub queued_at: u64,
    /// Milliseconds spent waiting for a turn / run-queue slot.
    #[serde(default)]
    pub wait_ms: u64,
    /// Milliseconds spent executing the check.
    #[serde(default)]
    pub run_ms: u64,
    #[serde(default)]
    pub succeeded: bool,
}

/// Persisted lifecycle and resource record for one dispatched issue (#619).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetIssueRecord {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub issue: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Extracted `Target metric:` line from the issue body for post-land evaluation (#621).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_metric: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<u32>,
    /// UTC Unix seconds when the issue was filed on GitHub or Collapse.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filed_at: Option<u64>,
    /// UTC Unix seconds when the issue was first dispatched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_at: Option<u64>,
    /// UTC Unix seconds of the most recent dispatch attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_dispatched_at: Option<u64>,
    /// UTC Unix seconds of the first snapshot observed for this issue's change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_snapshot_at: Option<u64>,
    /// UTC Unix seconds when the change was named (`rosen commit -m`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub named_at: Option<u64>,
    /// UTC Unix seconds when the change landed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_at: Option<u64>,
    /// UTC Unix seconds when post-land verification turned green for this land.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_land_green_at: Option<u64>,
    /// UTC Unix seconds when post-land failed or the change was reverted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_or_reverted_at: Option<u64>,
    /// UTC Unix seconds when the issue was closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub tokens_in: u64,
    #[serde(default)]
    pub tokens_out: u64,
    #[serde(default)]
    pub agent_wall_ms: u64,
    #[serde(default)]
    pub turns: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<FleetCheckRecord>,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub post_land_failures: u32,
    #[serde(default)]
    pub reverts: u32,
    #[serde(default)]
    pub review_findings: u32,
    /// Deduplication markers for post-land failures and reverts (Rosen event sequence).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_failure_events: Vec<u64>,
    /// Deduplication markers for review findings (e.g. `land:<commit>` or `seq:<n>`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seen_review_targets: Vec<String>,
}

impl FleetIssueRecord {
    pub fn new(repository: String, issue: u32) -> Self {
        Self {
            repository,
            issue,
            ..Self::default()
        }
    }

    pub fn filed_to_landed_secs(&self) -> Option<u64> {
        let filed = self.filed_at?;
        let landed = self.landed_at?;
        (landed >= filed).then_some(landed.saturating_sub(filed))
    }

    pub fn dispatched_to_landed_secs(&self) -> Option<u64> {
        let dispatched = self.dispatched_at?;
        let landed = self.landed_at?;
        (landed >= dispatched).then_some(landed.saturating_sub(dispatched))
    }

    pub fn total_tokens(&self) -> u64 {
        self.tokens_in.saturating_add(self.tokens_out)
    }

    /// Most recent lifecycle timestamp for window inclusion and pruning.
    pub fn latest_activity_at(&self) -> u64 {
        [
            self.closed_at,
            self.post_land_green_at,
            self.failed_or_reverted_at,
            self.landed_at,
            self.named_at,
            self.first_snapshot_at,
            self.last_dispatched_at,
            self.dispatched_at,
            self.filed_at,
        ]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(0)
    }

    /// Appends or updates a queued check record, deduplicating by `id`.
    pub fn record_check(&mut self, check: FleetCheckRecord) -> bool {
        if check.id.is_empty() {
            return false;
        }
        if let Some(existing) = self.checks.iter_mut().find(|item| item.id == check.id) {
            if *existing == check {
                return false;
            }
            *existing = check;
            return true;
        }
        self.checks.push(check);
        let excess = self.checks.len().saturating_sub(MAX_CHECKS_PER_ISSUE);
        if excess > 0 {
            self.checks.drain(..excess);
        }
        true
    }
}

/// Persisted per-project fleet ledger (#619).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetLedger {
    /// One record per dispatched issue, keyed by `issue_key(repository, issue)`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub issues: BTreeMap<String, FleetIssueRecord>,
    /// Idle slot-seconds accumulated per UTC hour start (Unix seconds, multiple of 3600).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub idle_slot_seconds_by_hour: BTreeMap<u64, u64>,
    /// Measured post-land outcomes for issues with a `Target metric:` line (#621).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub improvement_outcomes: Vec<ImprovementOutcomeRecord>,
}

impl FleetLedger {
    /// Records idle slot-seconds across `[start_secs, end_secs)` for `free_slots`, splitting
    /// accurately across UTC hour boundaries.
    pub fn record_idle_interval(
        &mut self,
        start_secs: u64,
        end_secs: u64,
        free_slots: u32,
    ) -> bool {
        if free_slots == 0 || end_secs <= start_secs {
            return false;
        }
        let slots = u64::from(free_slots);
        let mut cursor = start_secs;
        while cursor < end_secs {
            let bucket = hour_start_utc(cursor);
            let next_hour = bucket.saturating_add(HOUR_SECONDS);
            let slice_end = end_secs.min(next_hour);
            let delta = slice_end.saturating_sub(cursor).saturating_mul(slots);
            let entry = self.idle_slot_seconds_by_hour.entry(bucket).or_default();
            *entry = entry.saturating_add(delta);
            cursor = slice_end;
        }
        self.prune_idle_hours();
        true
    }

    pub fn prune_idle_hours(&mut self) {
        while self.idle_slot_seconds_by_hour.len() > MAX_IDLE_HOURS {
            if let Some(&oldest) = self.idle_slot_seconds_by_hour.keys().next() {
                self.idle_slot_seconds_by_hour.remove(&oldest);
            } else {
                break;
            }
        }
    }

    pub fn prune_issues(&mut self) {
        if self.issues.len() <= MAX_ISSUES_PER_PROJECT {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self
            .issues
            .iter()
            .map(|(key, record)| (record.latest_activity_at(), key.clone()))
            .collect();
        by_age.sort_unstable();
        let excess = self.issues.len().saturating_sub(MAX_ISSUES_PER_PROJECT);
        for (_, key) in by_age.into_iter().take(excess) {
            self.issues.remove(&key);
        }
    }

    pub fn find_by_change_mut(&mut self, change: &str) -> Option<&mut FleetIssueRecord> {
        self.issues
            .values_mut()
            .find(|record| record.change.as_deref() == Some(change))
    }

    pub fn find_by_thread_mut(&mut self, thread_id: u32) -> Option<&mut FleetIssueRecord> {
        self.issues
            .values_mut()
            .find(|record| record.thread_id == Some(thread_id))
    }

    pub fn find_by_commit_mut(&mut self, commit: &str) -> Option<&mut FleetIssueRecord> {
        self.issues
            .values_mut()
            .find(|record| record.landed_commit.as_deref() == Some(commit))
    }

    /// Inserts or updates a measured self-improvement outcome, keeping the newest records bounded (#621).
    pub fn upsert_improvement_outcome(&mut self, outcome: ImprovementOutcomeRecord) -> bool {
        if let Some(existing) = self
            .improvement_outcomes
            .iter_mut()
            .find(|item| item.matches_land(&outcome.repository, outcome.issue, outcome.landed_at))
        {
            if *existing == outcome {
                return false;
            }
            *existing = outcome;
            return true;
        }
        self.improvement_outcomes.push(outcome);
        self.improvement_outcomes.sort_by(|left, right| {
            right
                .landed_at
                .cmp(&left.landed_at)
                .then_with(|| right.issue.cmp(&left.issue))
        });
        self.improvement_outcomes
            .truncate(crate::improvement_outcomes::MAX_IMPROVEMENT_OUTCOMES);
        true
    }
}

/// Hourly rollup metrics for one project and one UTC hour (#619).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetHourRollup {
    pub hour_start: u64,
    pub lands: u32,
    pub issues_dispatched: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_filed_to_landed_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_dispatched_to_landed_secs: Option<u64>,
    pub idle_slot_seconds: u64,
    pub idle_slot_minutes: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_landed_change: Option<u64>,
    pub post_land_failures: u32,
    pub reverts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_land_failure_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revert_rate: Option<f64>,
    pub checks_queued: u32,
    pub check_queue_wait_ms_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_queue_wait_ms_median: Option<u64>,
    pub check_run_ms_total: u64,
    pub retries: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries_per_issue: Option<f64>,
    pub review_findings: u32,
}

/// Counts-and-durations-only hourly rollup sent to Collapse's state feed (#619, collapse.sh#225).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetCollapseHourRollup {
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

impl From<&FleetHourRollup> for FleetCollapseHourRollup {
    fn from(hour: &FleetHourRollup) -> Self {
        Self {
            hour_start: hour.hour_start,
            lands: hour.lands,
            issues_dispatched: hour.issues_dispatched,
            median_filed_to_landed_secs: hour.median_filed_to_landed_secs,
            median_dispatched_to_landed_secs: hour.median_dispatched_to_landed_secs,
            idle_slot_seconds: hour.idle_slot_seconds,
            tokens_in: hour.tokens_in,
            tokens_out: hour.tokens_out,
            tokens_per_landed_change: hour.tokens_per_landed_change,
            post_land_failures: hour.post_land_failures,
            reverts: hour.reverts,
            checks_queued: hour.checks_queued,
            check_queue_wait_ms_total: hour.check_queue_wait_ms_total,
            check_queue_wait_ms_median: hour.check_queue_wait_ms_median,
            check_run_ms_total: hour.check_run_ms_total,
            retries: hour.retries,
            review_findings: hour.review_findings,
        }
    }
}

/// One worst-offender issue row returned by `fleet_stats` and shown in the Fleet status section.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetOffender {
    pub repository: String,
    pub issue: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dispatched_to_landed_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filed_to_landed_secs: Option<u64>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub retries: u32,
    pub restarts: u32,
    pub reverts: u32,
    pub review_findings: u32,
}

impl FleetOffender {
    fn from_record(record: &FleetIssueRecord) -> Self {
        Self {
            repository: record.repository.clone(),
            issue: record.issue,
            title: record.title.clone(),
            url: record.url.clone(),
            thread_id: record.thread_id,
            dispatched_to_landed_secs: record.dispatched_to_landed_secs(),
            filed_to_landed_secs: record.filed_to_landed_secs(),
            tokens_in: record.tokens_in,
            tokens_out: record.tokens_out,
            retries: record.retries,
            restarts: record.restarts,
            reverts: record.reverts,
            review_findings: record.review_findings,
        }
    }
}

/// Top offenders in each failure/cost dimension within a window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FleetOffenders {
    pub slowest_issues: Vec<FleetOffender>,
    pub most_tokens: Vec<FleetOffender>,
    pub most_retries: Vec<FleetOffender>,
}

/// Rolled-up summary for one project over a time window (e.g. 24h or 7d).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FleetWindowSummary {
    pub window_hours: u32,
    pub from_unix: u64,
    pub to_unix: u64,
    pub lands: u32,
    pub issues_dispatched: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_filed_to_landed_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub median_dispatched_to_landed_secs: Option<u64>,
    pub idle_slot_seconds: u64,
    pub idle_slot_minutes: f64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_landed_change: Option<u64>,
    pub post_land_failures: u32,
    pub reverts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_land_failure_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revert_rate: Option<f64>,
    pub checks_queued: u32,
    pub check_queue_wait_ms_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check_queue_wait_ms_median: Option<u64>,
    pub check_run_ms_total: u64,
    pub retries_total: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retries_per_issue: Option<f64>,
    pub review_findings: u32,
    pub hours: Vec<FleetHourRollup>,
    pub offenders: FleetOffenders,
}

#[derive(Default)]
struct HourAccumulator {
    lands: u32,
    issues_dispatched: u32,
    filed_to_landed: Vec<u64>,
    dispatched_to_landed: Vec<u64>,
    idle_slot_seconds: u64,
    tokens_in: u64,
    tokens_out: u64,
    post_land_failures: u32,
    reverts: u32,
    checks_queued: u32,
    check_waits: Vec<u64>,
    check_wait_total: u64,
    check_run_total: u64,
    retries: u32,
    review_findings: u32,
}

/// Computes the exact window summary, per-hour rollups, and top offenders from `ledger`.
pub fn build_window_summary(
    ledger: &FleetLedger,
    window_hours: u32,
    now_secs: u64,
) -> FleetWindowSummary {
    let window_hours = window_hours.max(1);
    let current_hour = hour_start_utc(now_secs);
    let from_unix = current_hour.saturating_sub(u64::from(window_hours - 1) * HOUR_SECONDS);
    let to_unix = current_hour.saturating_add(HOUR_SECONDS);
    let mut buckets = BTreeMap::<u64, HourAccumulator>::new();
    for (&hour, &idle_secs) in &ledger.idle_slot_seconds_by_hour {
        if hour >= from_unix && hour < to_unix && idle_secs > 0 {
            buckets.entry(hour).or_default().idle_slot_seconds = idle_secs;
        }
    }
    let mut active_issues = Vec::new();
    let mut all_filed_to_landed = Vec::new();
    let mut all_dispatched_to_landed = Vec::new();
    let mut all_check_waits = Vec::new();
    for record in ledger.issues.values() {
        let touched = accumulate_issue(
            record,
            from_unix,
            to_unix,
            &mut buckets,
            &mut all_filed_to_landed,
            &mut all_dispatched_to_landed,
            &mut all_check_waits,
        );
        if touched {
            active_issues.push(record);
        }
    }
    let hours: Vec<FleetHourRollup> = buckets
        .into_iter()
        .map(|(hour_start, acc)| finalize_hour(hour_start, acc))
        .collect();
    finalize_window(
        window_hours,
        from_unix,
        to_unix,
        hours,
        active_issues,
        all_filed_to_landed,
        all_dispatched_to_landed,
        all_check_waits,
    )
}

fn accumulate_issue(
    record: &FleetIssueRecord,
    from_unix: u64,
    to_unix: u64,
    buckets: &mut BTreeMap<u64, HourAccumulator>,
    all_filed_to_landed: &mut Vec<u64>,
    all_dispatched_to_landed: &mut Vec<u64>,
    all_check_waits: &mut Vec<u64>,
) -> bool {
    let mut in_window = false;
    let dispatch_ts = record.last_dispatched_at.or(record.dispatched_at);
    if let Some(dispatched) = dispatch_ts
        && dispatched >= from_unix
        && dispatched < to_unix
    {
        in_window = true;
        let bucket = buckets.entry(hour_start_utc(dispatched)).or_default();
        bucket.issues_dispatched = bucket.issues_dispatched.saturating_add(1);
        bucket.retries = bucket.retries.saturating_add(record.retries);
    }
    if let Some(landed) = record.landed_at
        && landed >= from_unix
        && landed < to_unix
    {
        in_window = true;
        let bucket = buckets.entry(hour_start_utc(landed)).or_default();
        bucket.lands = bucket.lands.saturating_add(1);
        bucket.tokens_in = bucket.tokens_in.saturating_add(record.tokens_in);
        bucket.tokens_out = bucket.tokens_out.saturating_add(record.tokens_out);
        bucket.post_land_failures = bucket
            .post_land_failures
            .saturating_add(record.post_land_failures);
        bucket.reverts = bucket.reverts.saturating_add(record.reverts);
        bucket.review_findings = bucket
            .review_findings
            .saturating_add(record.review_findings);
        if let Some(duration) = record.filed_to_landed_secs() {
            bucket.filed_to_landed.push(duration);
            all_filed_to_landed.push(duration);
        }
        if let Some(duration) = record.dispatched_to_landed_secs() {
            bucket.dispatched_to_landed.push(duration);
            all_dispatched_to_landed.push(duration);
        }
    } else if let Some(failed) = record.failed_or_reverted_at
        && failed >= from_unix
        && failed < to_unix
    {
        in_window = true;
        let bucket = buckets.entry(hour_start_utc(failed)).or_default();
        bucket.post_land_failures = bucket
            .post_land_failures
            .saturating_add(record.post_land_failures);
        bucket.reverts = bucket.reverts.saturating_add(record.reverts);
    }
    for check in &record.checks {
        if check.queued_at >= from_unix && check.queued_at < to_unix {
            in_window = true;
            let bucket = buckets.entry(hour_start_utc(check.queued_at)).or_default();
            bucket.checks_queued = bucket.checks_queued.saturating_add(1);
            bucket.check_wait_total = bucket.check_wait_total.saturating_add(check.wait_ms);
            bucket.check_run_total = bucket.check_run_total.saturating_add(check.run_ms);
            bucket.check_waits.push(check.wait_ms);
            all_check_waits.push(check.wait_ms);
        }
    }
    in_window
}

fn finalize_hour(hour_start: u64, mut acc: HourAccumulator) -> FleetHourRollup {
    let tokens_total = acc.tokens_in.saturating_add(acc.tokens_out);
    let tokens_per_landed_change = (acc.lands > 0).then(|| tokens_total / u64::from(acc.lands));
    let post_land_failure_rate =
        (acc.lands > 0).then(|| f64::from(acc.post_land_failures) / f64::from(acc.lands));
    let revert_rate = (acc.lands > 0).then(|| f64::from(acc.reverts) / f64::from(acc.lands));
    let retries_per_issue = (acc.issues_dispatched > 0)
        .then(|| f64::from(acc.retries) / f64::from(acc.issues_dispatched));
    FleetHourRollup {
        hour_start,
        lands: acc.lands,
        issues_dispatched: acc.issues_dispatched,
        median_filed_to_landed_secs: median_u64(&mut acc.filed_to_landed),
        median_dispatched_to_landed_secs: median_u64(&mut acc.dispatched_to_landed),
        idle_slot_seconds: acc.idle_slot_seconds,
        idle_slot_minutes: round_tenths(acc.idle_slot_seconds as f64 / 60.0),
        tokens_in: acc.tokens_in,
        tokens_out: acc.tokens_out,
        tokens_per_landed_change,
        post_land_failures: acc.post_land_failures,
        reverts: acc.reverts,
        post_land_failure_rate,
        revert_rate,
        checks_queued: acc.checks_queued,
        check_queue_wait_ms_total: acc.check_wait_total,
        check_queue_wait_ms_median: median_u64(&mut acc.check_waits),
        check_run_ms_total: acc.check_run_total,
        retries: acc.retries,
        retries_per_issue,
        review_findings: acc.review_findings,
    }
}

#[allow(clippy::too_many_arguments)]
fn finalize_window(
    window_hours: u32,
    from_unix: u64,
    to_unix: u64,
    hours: Vec<FleetHourRollup>,
    active_issues: Vec<&FleetIssueRecord>,
    mut all_filed_to_landed: Vec<u64>,
    mut all_dispatched_to_landed: Vec<u64>,
    mut all_check_waits: Vec<u64>,
) -> FleetWindowSummary {
    let lands: u32 = hours.iter().map(|hour| hour.lands).sum();
    let issues_dispatched: u32 = hours.iter().map(|hour| hour.issues_dispatched).sum();
    let idle_slot_seconds: u64 = hours.iter().map(|hour| hour.idle_slot_seconds).sum();
    let tokens_in: u64 = hours.iter().map(|hour| hour.tokens_in).sum();
    let tokens_out: u64 = hours.iter().map(|hour| hour.tokens_out).sum();
    let post_land_failures: u32 = hours.iter().map(|hour| hour.post_land_failures).sum();
    let reverts: u32 = hours.iter().map(|hour| hour.reverts).sum();
    let checks_queued: u32 = hours.iter().map(|hour| hour.checks_queued).sum();
    let check_queue_wait_ms_total: u64 = hours
        .iter()
        .map(|hour| hour.check_queue_wait_ms_total)
        .sum();
    let check_run_ms_total: u64 = hours.iter().map(|hour| hour.check_run_ms_total).sum();
    let retries_total: u32 = hours.iter().map(|hour| hour.retries).sum();
    let review_findings: u32 = hours.iter().map(|hour| hour.review_findings).sum();
    let tokens_total = tokens_in.saturating_add(tokens_out);
    let tokens_per_landed_change = (lands > 0).then(|| tokens_total / u64::from(lands));
    let post_land_failure_rate =
        (lands > 0).then(|| f64::from(post_land_failures) / f64::from(lands));
    let revert_rate = (lands > 0).then(|| f64::from(reverts) / f64::from(lands));
    let retries_denominator = issues_dispatched.max(active_issues.len() as u32);
    let retries_per_issue = (retries_denominator > 0)
        .then(|| f64::from(retries_total) / f64::from(retries_denominator));
    let offenders = build_offenders(&active_issues);
    FleetWindowSummary {
        window_hours,
        from_unix,
        to_unix,
        lands,
        issues_dispatched,
        median_filed_to_landed_secs: median_u64(&mut all_filed_to_landed),
        median_dispatched_to_landed_secs: median_u64(&mut all_dispatched_to_landed),
        idle_slot_seconds,
        idle_slot_minutes: round_tenths(idle_slot_seconds as f64 / 60.0),
        tokens_in,
        tokens_out,
        tokens_per_landed_change,
        post_land_failures,
        reverts,
        post_land_failure_rate,
        revert_rate,
        checks_queued,
        check_queue_wait_ms_total,
        check_queue_wait_ms_median: median_u64(&mut all_check_waits),
        check_run_ms_total,
        retries_total,
        retries_per_issue,
        review_findings,
        hours,
        offenders,
    }
}

fn build_offenders(issues: &[&FleetIssueRecord]) -> FleetOffenders {
    let mut slowest: Vec<&FleetIssueRecord> = issues
        .iter()
        .copied()
        .filter(|item| {
            item.dispatched_to_landed_secs().is_some() || item.filed_to_landed_secs().is_some()
        })
        .collect();
    slowest.sort_by(|left, right| {
        let left_key = left
            .dispatched_to_landed_secs()
            .or_else(|| left.filed_to_landed_secs())
            .unwrap_or(0);
        let right_key = right
            .dispatched_to_landed_secs()
            .or_else(|| right.filed_to_landed_secs())
            .unwrap_or(0);
        right_key
            .cmp(&left_key)
            .then_with(|| left.issue.cmp(&right.issue))
    });
    let mut by_tokens: Vec<&FleetIssueRecord> = issues
        .iter()
        .copied()
        .filter(|item| item.total_tokens() > 0)
        .collect();
    by_tokens.sort_by(|left, right| {
        right
            .total_tokens()
            .cmp(&left.total_tokens())
            .then_with(|| left.issue.cmp(&right.issue))
    });
    let mut by_retries: Vec<&FleetIssueRecord> = issues
        .iter()
        .copied()
        .filter(|item| item.retries > 0 || item.restarts > 0)
        .collect();
    by_retries.sort_by(|left, right| {
        right
            .retries
            .cmp(&left.retries)
            .then_with(|| right.restarts.cmp(&left.restarts))
            .then_with(|| left.issue.cmp(&right.issue))
    });
    FleetOffenders {
        slowest_issues: slowest
            .into_iter()
            .take(TOP_OFFENDERS_LIMIT)
            .map(FleetOffender::from_record)
            .collect(),
        most_tokens: by_tokens
            .into_iter()
            .take(TOP_OFFENDERS_LIMIT)
            .map(FleetOffender::from_record)
            .collect(),
        most_retries: by_retries
            .into_iter()
            .take(TOP_OFFENDERS_LIMIT)
            .map(FleetOffender::from_record)
            .collect(),
    }
}

/// Exact integer median (lower-middle average for even lengths).
pub fn median_u64(values: &mut [u64]) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        Some(values[mid])
    } else {
        let low = values[mid - 1];
        let high = values[mid];
        Some(low + (high - low) / 2)
    }
}

fn round_tenths(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// Parses a `fleet_stats` window specifier (`"24h"`, `"7d"`, `"168h"`, etc.) into hours.
pub fn parse_window_hours(raw: Option<&str>) -> Result<u32, String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(24);
    };
    let lower = raw.to_ascii_lowercase();
    let hours = if let Some(hours) = lower.strip_suffix('h') {
        hours
            .parse::<u32>()
            .map_err(|_| format!("Invalid window `{raw}`; expected e.g. `24h` or `7d`"))?
    } else if let Some(days) = lower.strip_suffix('d') {
        days.parse::<u32>()
            .map_err(|_| format!("Invalid window `{raw}`; expected e.g. `24h` or `7d`"))?
            .saturating_mul(24)
    } else {
        lower
            .parse::<u32>()
            .map_err(|_| format!("Invalid window `{raw}`; expected e.g. `24h` or `7d`"))?
    };
    if !(1..=24 * 90).contains(&hours) {
        return Err(format!(
            "Window `{raw}` is out of range; choose between 1h and 90d"
        ));
    }
    Ok(hours)
}

/// IPC-safe offender row for the status pane (`f64` avoids specta BigInt restrictions).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct FleetOffenderView {
    pub repository: String,
    pub issue: u32,
    pub title: Option<String>,
    pub url: Option<String>,
    pub thread_id: Option<u32>,
    pub dispatched_to_landed_secs: Option<f64>,
    pub filed_to_landed_secs: Option<f64>,
    pub tokens_total: f64,
    pub retries: u32,
    pub restarts: u32,
    pub reverts: u32,
    pub review_findings: u32,
}

impl From<&FleetOffender> for FleetOffenderView {
    fn from(item: &FleetOffender) -> Self {
        Self {
            repository: item.repository.clone(),
            issue: item.issue,
            title: item.title.clone(),
            url: item.url.clone(),
            thread_id: item.thread_id,
            dispatched_to_landed_secs: item.dispatched_to_landed_secs.map(|value| value as f64),
            filed_to_landed_secs: item.filed_to_landed_secs.map(|value| value as f64),
            tokens_total: item.tokens_in.saturating_add(item.tokens_out) as f64,
            retries: item.retries,
            restarts: item.restarts,
            reverts: item.reverts,
            review_findings: item.review_findings,
        }
    }
}

/// IPC-safe window summary for the status pane Fleet section (#619).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct FleetWindowView {
    pub window_hours: u32,
    pub lands: u32,
    pub issues_dispatched: u32,
    pub median_filed_to_landed_secs: Option<f64>,
    pub median_dispatched_to_landed_secs: Option<f64>,
    pub idle_slot_minutes: f64,
    pub tokens_total: f64,
    pub tokens_per_landed_change: Option<f64>,
    pub post_land_failures: u32,
    pub reverts: u32,
    pub post_land_failure_rate: Option<f64>,
    pub revert_rate: Option<f64>,
    pub checks_queued: u32,
    pub check_queue_wait_ms_median: Option<f64>,
    pub retries_total: u32,
    pub retries_per_issue: Option<f64>,
    pub review_findings: u32,
    pub slowest_issues: Vec<FleetOffenderView>,
    pub most_tokens: Vec<FleetOffenderView>,
    pub most_retries: Vec<FleetOffenderView>,
}

impl From<&FleetWindowSummary> for FleetWindowView {
    fn from(summary: &FleetWindowSummary) -> Self {
        Self {
            window_hours: summary.window_hours,
            lands: summary.lands,
            issues_dispatched: summary.issues_dispatched,
            median_filed_to_landed_secs: summary
                .median_filed_to_landed_secs
                .map(|value| value as f64),
            median_dispatched_to_landed_secs: summary
                .median_dispatched_to_landed_secs
                .map(|value| value as f64),
            idle_slot_minutes: summary.idle_slot_minutes,
            tokens_total: summary.tokens_in.saturating_add(summary.tokens_out) as f64,
            tokens_per_landed_change: summary.tokens_per_landed_change.map(|value| value as f64),
            post_land_failures: summary.post_land_failures,
            reverts: summary.reverts,
            post_land_failure_rate: summary.post_land_failure_rate,
            revert_rate: summary.revert_rate,
            checks_queued: summary.checks_queued,
            check_queue_wait_ms_median: summary
                .check_queue_wait_ms_median
                .map(|value| value as f64),
            retries_total: summary.retries_total,
            retries_per_issue: summary.retries_per_issue,
            review_findings: summary.review_findings,
            slowest_issues: summary
                .offenders
                .slowest_issues
                .iter()
                .map(FleetOffenderView::from)
                .collect(),
            most_tokens: summary
                .offenders
                .most_tokens
                .iter()
                .map(FleetOffenderView::from)
                .collect(),
            most_retries: summary
                .offenders
                .most_retries
                .iter()
                .map(FleetOffenderView::from)
                .collect(),
        }
    }
}

/// Complete Fleet status payload for the status pane (last 24 hours, last 7 days, and recent improvements, #619, #621).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct FleetStatusView {
    pub last_24h: FleetWindowView,
    pub last_7d: FleetWindowView,
    pub recent_improvements: Vec<FleetImprovementOutcomeView>,
}

impl FleetStatusView {
    pub fn from_ledger(ledger: &FleetLedger, now_secs: u64) -> Self {
        let last_24h = build_window_summary(ledger, 24, now_secs);
        let last_7d = build_window_summary(ledger, 24 * 7, now_secs);
        let recent_improvements = build_recent_improvement_views(ledger, now_secs);
        Self {
            last_24h: FleetWindowView::from(&last_24h),
            last_7d: FleetWindowView::from(&last_7d),
            recent_improvements,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_interval_splits_across_hour_boundaries() {
        let mut ledger = FleetLedger::default();
        // 10 seconds before hour 2 through 20 seconds into hour 2 with 3 free slots.
        let hour2 = 2 * HOUR_SECONDS;
        assert!(ledger.record_idle_interval(hour2 - 10, hour2 + 20, 3));
        assert_eq!(
            ledger.idle_slot_seconds_by_hour.get(&HOUR_SECONDS),
            Some(&30)
        );
        assert_eq!(ledger.idle_slot_seconds_by_hour.get(&hour2), Some(&60));
    }

    #[test]
    fn hourly_rollup_and_offenders_compute_medians_rates_and_tokens() {
        let mut ledger = FleetLedger::default();
        let base_hour = 1_795_262_400; // exact UTC hour
        let mut fast = FleetIssueRecord::new("mESmaC/velocity".into(), 418);
        fast.filed_at = Some(base_hour - 1_200);
        fast.dispatched_at = Some(base_hour + 60);
        fast.landed_at = Some(base_hour + 1_080); // 1020s (17m) from dispatch
        fast.tokens_in = 80_000;
        fast.tokens_out = 20_000;
        fast.checks.push(FleetCheckRecord {
            id: "run-1".into(),
            command: "preflight".into(),
            queued_at: base_hour + 900,
            wait_ms: 4_000,
            run_ms: 15_000,
            succeeded: true,
        });
        ledger
            .issues
            .insert(issue_key("mESmaC/velocity", 418), fast);

        let mut slow = FleetIssueRecord::new("mESmaC/velocity".into(), 619);
        slow.filed_at = Some(base_hour - 3_600);
        slow.dispatched_at = Some(base_hour + 100);
        slow.landed_at = Some(base_hour + 3_100); // 3000s from dispatch
        slow.tokens_in = 250_000;
        slow.tokens_out = 50_000;
        slow.retries = 2;
        slow.reverts = 1;
        slow.post_land_failures = 1;
        slow.review_findings = 1;
        ledger
            .issues
            .insert(issue_key("mESmaC/velocity", 619), slow);

        ledger.record_idle_interval(base_hour, base_hour + 600, 2); // 1200 slot-secs = 20 slot-min

        let summary = build_window_summary(&ledger, 24, base_hour + 3_500);
        assert_eq!(summary.lands, 2);
        assert_eq!(summary.median_dispatched_to_landed_secs, Some(2_010));
        assert_eq!(summary.tokens_per_landed_change, Some(200_000));
        assert_eq!(summary.idle_slot_minutes, 20.0);
        assert_eq!(summary.post_land_failure_rate, Some(0.5));
        assert_eq!(summary.revert_rate, Some(0.5));
        assert_eq!(summary.check_queue_wait_ms_median, Some(4_000));
        assert_eq!(summary.retries_per_issue, Some(1.0));
        assert_eq!(summary.offenders.slowest_issues[0].issue, 619);
        assert_eq!(summary.offenders.most_tokens[0].issue, 619);
        assert_eq!(summary.offenders.most_retries[0].issue, 619);

        let feed_item = FleetCollapseHourRollup::from(&summary.hours[0]);
        assert_eq!(feed_item.hour_start, base_hour);
        assert_eq!(feed_item.lands, 2);
        assert_eq!(feed_item.idle_slot_seconds, 1_200);
        assert_eq!(feed_item.tokens_per_landed_change, Some(200_000));
        assert_eq!(feed_item.reverts, 1);
        assert_eq!(feed_item.post_land_failures, 1);
        assert_eq!(feed_item.checks_queued, 1);
        assert_eq!(feed_item.check_queue_wait_ms_median, Some(4_000));
        assert_eq!(feed_item.retries, 2);
        assert_eq!(feed_item.review_findings, 1);
    }

    #[test]
    fn parse_window_hours_accepts_hours_and_days() {
        assert_eq!(parse_window_hours(None), Ok(24));
        assert_eq!(parse_window_hours(Some("24h")), Ok(24));
        assert_eq!(parse_window_hours(Some("7d")), Ok(168));
        assert!(parse_window_hours(Some("0h")).is_err());
        assert!(parse_window_hours(Some("bad")).is_err());
    }
}
