//! Post-land metric evaluation for self-improvement issues (#621).
//!
//! For each landed issue with a `Target metric:` line, compares that `fleet_stats` metric
//! in the same project before and after the land over a window of 24 hours or 20 lands,
//! whichever is longer. Produces idempotent issue comments, persisted outcome records for
//! the improvement scout (#622), Fleet status rows, and follow-up `self-improvement`
//! regression issue drafts without reverting automatically.

use serde::{Deserialize, Serialize};

use crate::fleet_ledger::{FleetIssueRecord, FleetLedger, HOUR_SECONDS, issue_key, median_u64};
use crate::improvement_scout::SELF_IMPROVEMENT_LABEL;
use crate::text_budget::preview;

/// Minimum elapsed time after a land before its post-land window can close (24 hours).
pub const OUTCOME_WINDOW_MIN_HOURS: u64 = 24;
/// Minimum elapsed seconds after a land before its post-land window can close.
pub const OUTCOME_WINDOW_MIN_SECONDS: u64 = OUTCOME_WINDOW_MIN_HOURS * HOUR_SECONDS;
/// Minimum number of subsequent lands in the same project before the window can close.
pub const OUTCOME_WINDOW_MIN_LANDS: usize = 20;
/// Relative worsening (25%) required to flag a clear regression and file a follow-up issue.
pub const REGRESSION_THRESHOLD_RATIO: f64 = 0.25;
/// Relative improvement (10%) required to classify an outcome as improved.
pub const IMPROVEMENT_THRESHOLD_RATIO: f64 = 0.10;
/// Minimum samples on each side (before and after) required for a conclusive verdict.
pub const MIN_SAMPLES_PER_SIDE: u32 = 10;
/// Fixed-point multiplier used to store metric values as exact `Eq`-compatible integers.
pub const METRIC_SCALE: f64 = 10_000.0;
/// Retain up to 200 most recent improvement outcome records per project.
pub const MAX_IMPROVEMENT_OUTCOMES: usize = 200;
/// Maximum recent improvements (pending + measured) shown in the Fleet status pane.
pub const RECENT_IMPROVEMENTS_LIMIT: usize = 10;

const RATIO_EPSILON: f64 = 1e-9;

/// Desired direction of movement for a target metric.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TargetDirection {
    Down,
    Up,
}

impl TargetDirection {
    pub fn description(self) -> &'static str {
        match self {
            Self::Down => "lower is better",
            Self::Up => "higher is better",
        }
    }
}

/// Supported `fleet_stats` metrics that a `Target metric:` line can target (#619, #621).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum TargetMetricKind {
    IdleSlotMinutesPerDay,
    IdleSlotMinutesPerHour,
    LandsPerHour,
    LandsPerDay,
    MedianDispatchedToLandedSecs,
    MedianFiledToLandedSecs,
    TokensPerLandedChange,
    PostLandFailureRate,
    RevertRate,
    CheckQueueWaitMsMedian,
    RetriesPerIssue,
    ReviewFindingsPerLand,
}

impl TargetMetricKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::IdleSlotMinutesPerDay => "Idle slot-minutes per day",
            Self::IdleSlotMinutesPerHour => "Idle slot-minutes per hour",
            Self::LandsPerHour => "Lands per hour",
            Self::LandsPerDay => "Lands per day",
            Self::MedianDispatchedToLandedSecs => "Median dispatch → land time",
            Self::MedianFiledToLandedSecs => "Median filed → land time",
            Self::TokensPerLandedChange => "Tokens per landed change",
            Self::PostLandFailureRate => "Post-land failure rate",
            Self::RevertRate => "Revert rate",
            Self::CheckQueueWaitMsMedian => "Median check queue wait",
            Self::RetriesPerIssue => "Retries per issue",
            Self::ReviewFindingsPerLand => "Review findings per land",
        }
    }

    pub fn sample_unit(self) -> &'static str {
        match self {
            Self::IdleSlotMinutesPerDay | Self::IdleSlotMinutesPerHour => "hours",
            Self::LandsPerHour
            | Self::LandsPerDay
            | Self::MedianDispatchedToLandedSecs
            | Self::MedianFiledToLandedSecs
            | Self::TokensPerLandedChange
            | Self::PostLandFailureRate
            | Self::RevertRate
            | Self::ReviewFindingsPerLand => "lands",
            Self::CheckQueueWaitMsMedian => "checks",
            Self::RetriesPerIssue => "issues",
        }
    }

    pub fn natural_direction(self) -> TargetDirection {
        match self {
            Self::LandsPerHour | Self::LandsPerDay => TargetDirection::Up,
            _ => TargetDirection::Down,
        }
    }

    /// Absolute post-land floor that counts as a clear regression when `before == 0.0`
    /// and `direction == Down` (so `0% -> 30%` reverts triggers a regression follow-up).
    fn zero_baseline_regression_floor(self) -> f64 {
        match self {
            Self::PostLandFailureRate
            | Self::RevertRate
            | Self::RetriesPerIssue
            | Self::ReviewFindingsPerLand => REGRESSION_THRESHOLD_RATIO,
            Self::IdleSlotMinutesPerDay => 30.0,
            Self::IdleSlotMinutesPerHour => 5.0,
            Self::MedianDispatchedToLandedSecs | Self::MedianFiledToLandedSecs => 60.0,
            Self::TokensPerLandedChange => 10_000.0,
            Self::CheckQueueWaitMsMedian => 5_000.0,
            Self::LandsPerHour | Self::LandsPerDay => 0.25,
        }
    }
}

/// Parsed metric and target direction from an issue's `Target metric:` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedTargetMetric {
    pub kind: TargetMetricKind,
    pub direction: TargetDirection,
}

/// Measured verdict for one self-improvement change after its window closes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum ImprovementVerdict {
    Improved,
    #[default]
    NoSignificantChange,
    Regressed,
    InsufficientSamples,
    UnsupportedMetric,
}

impl ImprovementVerdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Improved => "Improved",
            Self::NoSignificantChange => "No significant change",
            Self::Regressed => "Regressed",
            Self::InsufficientSamples => "Insufficient samples",
            Self::UnsupportedMetric => "Unmeasurable metric",
        }
    }
}

/// Persisted measured outcome for one landed issue with a `Target metric:` line (#621).
///
/// Uses fixed-point scaled integers so `FleetLedger` and `ProjectItem` keep exact `Eq`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImprovementOutcomeRecord {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub issue: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landed_commit: Option<String>,
    #[serde(default)]
    pub landed_at: u64,
    #[serde(default)]
    pub measured_at: u64,
    #[serde(default)]
    pub target_metric_raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<TargetMetricKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<TargetDirection>,
    #[serde(default)]
    pub before_from_unix: u64,
    #[serde(default)]
    pub before_to_unix: u64,
    #[serde(default)]
    pub after_from_unix: u64,
    #[serde(default)]
    pub after_to_unix: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_value_scaled: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_value_scaled: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_change_bps: Option<i32>,
    #[serde(default)]
    pub before_samples: u32,
    #[serde(default)]
    pub after_samples: u32,
    #[serde(default)]
    pub before_lands: u32,
    #[serde(default)]
    pub after_lands: u32,
    #[serde(default)]
    pub verdict: ImprovementVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commented_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_issue: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_issue_url: Option<String>,
}

impl ImprovementOutcomeRecord {
    pub fn before_value(&self) -> Option<f64> {
        self.before_value_scaled
            .map(|value| value as f64 / METRIC_SCALE)
    }

    pub fn after_value(&self) -> Option<f64> {
        self.after_value_scaled
            .map(|value| value as f64 / METRIC_SCALE)
    }

    pub fn relative_change_pct(&self) -> Option<f64> {
        self.relative_change_bps.map(|bps| f64::from(bps) / 100.0)
    }

    pub fn matches_land(&self, repository: &str, issue: u32, landed_at: u64) -> bool {
        self.repository.eq_ignore_ascii_case(repository)
            && self.issue == issue
            && self.landed_at == landed_at
    }

    pub fn land_token(&self) -> String {
        self.landed_commit
            .as_deref()
            .map(str::trim)
            .filter(|commit| !commit.is_empty())
            .map(|commit| commit.chars().take(12).collect())
            .unwrap_or_else(|| self.landed_at.to_string())
    }
}

/// IPC-safe improvement row shown in the Fleet section of the status pane (#621).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct FleetImprovementOutcomeView {
    pub repository: String,
    pub issue: u32,
    pub title: Option<String>,
    pub url: Option<String>,
    pub target_metric: String,
    pub metric_label: String,
    pub verdict: ImprovementVerdict,
    pub before_text: String,
    pub after_text: String,
    pub change_text: String,
    pub before_samples: u32,
    pub after_samples: u32,
    pub landed_at_secs: f64,
    pub measured_at_secs: f64,
    pub regression_issue: Option<u32>,
    pub regression_issue_url: Option<String>,
    pub pending: bool,
    pub pending_summary: Option<String>,
}

/// Ready-to-file follow-up issue draft when a self-improvement change clearly regressed (#621).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegressionFollowUpDraft {
    pub title: String,
    pub body: String,
    pub label: String,
    pub marker: String,
}

/// Parses a `Target metric:` value into a known `fleet_stats` metric and direction.
pub fn parse_target_metric_spec(raw: &str) -> Option<ParsedTargetMetric> {
    let cleaned = raw.trim().replace("**", "");
    let lower = cleaned.to_ascii_lowercase();
    let kind = classify_metric_kind(&lower)?;
    let direction = classify_direction(&lower).unwrap_or_else(|| kind.natural_direction());
    Some(ParsedTargetMetric { kind, direction })
}

fn classify_metric_kind(lower: &str) -> Option<TargetMetricKind> {
    if lower.contains("idle") {
        return Some(
            if lower.contains("per hour") || lower.contains("/hr") || lower.contains("/hour") {
                TargetMetricKind::IdleSlotMinutesPerHour
            } else {
                TargetMetricKind::IdleSlotMinutesPerDay
            },
        );
    }
    if lower.contains("revert") {
        return Some(TargetMetricKind::RevertRate);
    }
    if lower.contains("post-land fail")
        || lower.contains("post land fail")
        || lower.contains("failure rate")
        || lower.contains("integration fail")
    {
        return Some(TargetMetricKind::PostLandFailureRate);
    }
    if lower.contains("check") && lower.contains("wait")
        || lower.contains("queue wait")
        || lower.contains("preflight wait")
    {
        return Some(TargetMetricKind::CheckQueueWaitMsMedian);
    }
    if lower.contains("token") {
        return Some(TargetMetricKind::TokensPerLandedChange);
    }
    if lower.contains("retr") || lower.contains("restart") {
        return Some(TargetMetricKind::RetriesPerIssue);
    }
    if lower.contains("review finding") {
        return Some(TargetMetricKind::ReviewFindingsPerLand);
    }
    if lower.contains("filed") && (lower.contains("land") || lower.contains("time"))
        || lower.contains("lead time")
    {
        return Some(TargetMetricKind::MedianFiledToLandedSecs);
    }
    if lower.contains("dispatch") && (lower.contains("land") || lower.contains("time"))
        || lower.contains("cycle time")
        || lower.contains("land time")
    {
        return Some(TargetMetricKind::MedianDispatchedToLandedSecs);
    }
    if lower.contains("land") || lower.contains("throughput") {
        return Some(
            if lower.contains("per day") || lower.contains("/day") || lower.contains("daily") {
                TargetMetricKind::LandsPerDay
            } else {
                TargetMetricKind::LandsPerHour
            },
        );
    }
    None
}

fn classify_direction(lower: &str) -> Option<TargetDirection> {
    let words: Vec<&str> = lower
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    for word in words.into_iter().rev() {
        match word {
            "down" | "lower" | "fewer" | "shorter" | "decrease" | "reduce" | "less" => {
                return Some(TargetDirection::Down);
            }
            "up" | "higher" | "more" | "increase" => {
                return Some(TargetDirection::Up);
            }
            _ => {}
        }
    }
    None
}

/// Evaluates every landed issue with a `Target metric:` line whose window has closed and
/// that does not yet have a persisted outcome record.
pub fn evaluate_pending_outcomes(
    ledger: &FleetLedger,
    now_secs: u64,
) -> Vec<ImprovementOutcomeRecord> {
    let mut results = Vec::new();
    for record in ledger.issues.values() {
        let Some(landed_at) = record.landed_at else {
            continue;
        };
        if record
            .target_metric
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
        {
            continue;
        }
        let already_recorded = ledger
            .improvement_outcomes
            .iter()
            .any(|existing| existing.matches_land(&record.repository, record.issue, landed_at));
        if already_recorded {
            continue;
        }
        if let Some(outcome) = evaluate_issue_outcome(ledger, record, now_secs) {
            results.push(outcome);
        }
    }
    results.sort_by_key(|item| (item.landed_at, item.issue));
    results
}

/// Evaluates one landed issue against `ledger` if its post-land window
/// (`max(24 hours, 20 lands)`) has closed by `now_secs`.
pub fn evaluate_issue_outcome(
    ledger: &FleetLedger,
    target: &FleetIssueRecord,
    now_secs: u64,
) -> Option<ImprovementOutcomeRecord> {
    let landed_at = target.landed_at?;
    let target_metric_raw = target
        .target_metric
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let target_key = issue_key(&target.repository, target.issue);
    let (before_lands, after_lands) =
        partition_other_lands(ledger, &target_key, landed_at, now_secs);
    let min_time_end = landed_at.saturating_add(OUTCOME_WINDOW_MIN_SECONDS);
    if now_secs < min_time_end || after_lands.len() < OUTCOME_WINDOW_MIN_LANDS {
        return None;
    }
    let after_end = min_time_end.max(after_lands[OUTCOME_WINDOW_MIN_LANDS - 1].landed_at?);
    let before_start = compute_before_start(landed_at, &before_lands);
    let before_count = before_lands
        .iter()
        .filter(|item| item.landed_at.is_some_and(|ts| ts >= before_start))
        .count() as u32;
    let after_count = after_lands
        .iter()
        .filter(|item| item.landed_at.is_some_and(|ts| ts <= after_end))
        .count() as u32;
    let parsed = parse_target_metric_spec(&target_metric_raw);
    let measurement = measure_comparison(
        ledger,
        &target_key,
        parsed,
        before_start,
        landed_at,
        after_end,
    );
    Some(ImprovementOutcomeRecord {
        repository: target.repository.clone(),
        issue: target.issue,
        title: target.title.clone(),
        url: target.url.clone(),
        change: target.change.clone(),
        landed_commit: target.landed_commit.clone(),
        landed_at,
        measured_at: now_secs,
        target_metric_raw,
        metric: parsed.map(|spec| spec.kind),
        direction: parsed.map(|spec| spec.direction),
        before_from_unix: before_start,
        before_to_unix: landed_at,
        after_from_unix: landed_at,
        after_to_unix: after_end,
        before_value_scaled: measurement.before_value_scaled,
        after_value_scaled: measurement.after_value_scaled,
        relative_change_bps: measurement.relative_change_bps,
        before_samples: measurement.before_samples,
        after_samples: measurement.after_samples,
        before_lands: before_count,
        after_lands: after_count,
        verdict: measurement.verdict,
        commented_at: None,
        regression_issue: None,
        regression_issue_url: None,
    })
}

struct ComparisonMeasurement {
    before_value_scaled: Option<i64>,
    after_value_scaled: Option<i64>,
    relative_change_bps: Option<i32>,
    before_samples: u32,
    after_samples: u32,
    verdict: ImprovementVerdict,
}

fn measure_comparison(
    ledger: &FleetLedger,
    target_key: &str,
    parsed: Option<ParsedTargetMetric>,
    before_start: u64,
    landed_at: u64,
    after_end: u64,
) -> ComparisonMeasurement {
    let Some(spec) = parsed else {
        return ComparisonMeasurement {
            before_value_scaled: None,
            after_value_scaled: None,
            relative_change_bps: None,
            before_samples: 0,
            after_samples: 0,
            verdict: ImprovementVerdict::UnsupportedMetric,
        };
    };
    let earliest = earliest_ledger_timestamp(ledger);
    let before = measure_metric(
        ledger,
        target_key,
        spec.kind,
        WindowSlice {
            from_unix: before_start,
            to_unix: landed_at,
            inclusive_end: false,
            observed_from_floor: earliest,
        },
    );
    let after = measure_metric(
        ledger,
        target_key,
        spec.kind,
        WindowSlice {
            from_unix: landed_at,
            to_unix: after_end,
            inclusive_end: true,
            observed_from_floor: None,
        },
    );
    let (verdict, relative_change_bps) = classify_verdict(spec, &before, &after);
    ComparisonMeasurement {
        before_value_scaled: before.value.map(scale_value),
        after_value_scaled: after.value.map(scale_value),
        relative_change_bps,
        before_samples: before.samples,
        after_samples: after.samples,
        verdict,
    }
}

fn partition_other_lands<'a>(
    ledger: &'a FleetLedger,
    target_key: &str,
    landed_at: u64,
    now_secs: u64,
) -> (Vec<&'a FleetIssueRecord>, Vec<&'a FleetIssueRecord>) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for (key, record) in &ledger.issues {
        if key == target_key {
            continue;
        }
        let Some(ts) = record.landed_at else {
            continue;
        };
        if ts < landed_at {
            before.push(record);
        } else if ts <= now_secs {
            after.push(record);
        }
    }
    // Newest first for `before` so index 19 is the 20th most recent land before `landed_at`.
    before.sort_by(|left, right| {
        right
            .landed_at
            .cmp(&left.landed_at)
            .then_with(|| left.issue.cmp(&right.issue))
    });
    // Oldest first for `after` so index 19 is the 20th land after `landed_at`.
    after.sort_by(|left, right| {
        left.landed_at
            .cmp(&right.landed_at)
            .then_with(|| left.issue.cmp(&right.issue))
    });
    (before, after)
}

fn compute_before_start(landed_at: u64, before_lands: &[&FleetIssueRecord]) -> u64 {
    let min_time_start = landed_at.saturating_sub(OUTCOME_WINDOW_MIN_SECONDS);
    let land_bound = if before_lands.len() >= OUTCOME_WINDOW_MIN_LANDS {
        before_lands[OUTCOME_WINDOW_MIN_LANDS - 1].landed_at
    } else {
        before_lands.last().and_then(|record| record.landed_at)
    };
    land_bound.map_or(min_time_start, |bound| min_time_start.min(bound))
}

#[derive(Clone, Copy)]
struct WindowSlice {
    from_unix: u64,
    to_unix: u64,
    inclusive_end: bool,
    observed_from_floor: Option<u64>,
}

impl WindowSlice {
    fn contains(self, ts: u64) -> bool {
        if self.inclusive_end {
            ts >= self.from_unix && ts <= self.to_unix
        } else {
            ts >= self.from_unix && ts < self.to_unix
        }
    }

    fn observed_span_secs(self) -> u64 {
        let effective_start = self
            .observed_from_floor
            .map_or(self.from_unix, |floor| self.from_unix.max(floor));
        self.to_unix.saturating_sub(effective_start)
    }
}

struct MetricSample {
    value: Option<f64>,
    samples: u32,
}

fn measure_metric(
    ledger: &FleetLedger,
    target_key: &str,
    kind: TargetMetricKind,
    slice: WindowSlice,
) -> MetricSample {
    match kind {
        TargetMetricKind::IdleSlotMinutesPerDay => measure_idle_rate(ledger, slice, 86_400.0),
        TargetMetricKind::IdleSlotMinutesPerHour => measure_idle_rate(ledger, slice, 3_600.0),
        TargetMetricKind::LandsPerHour => measure_land_rate(ledger, target_key, slice, 3_600.0),
        TargetMetricKind::LandsPerDay => measure_land_rate(ledger, target_key, slice, 86_400.0),
        TargetMetricKind::MedianDispatchedToLandedSecs => measure_land_median(
            ledger,
            target_key,
            slice,
            FleetIssueRecord::dispatched_to_landed_secs,
        ),
        TargetMetricKind::MedianFiledToLandedSecs => measure_land_median(
            ledger,
            target_key,
            slice,
            FleetIssueRecord::filed_to_landed_secs,
        ),
        TargetMetricKind::TokensPerLandedChange => {
            measure_land_ratio(ledger, target_key, slice, |record| {
                record.total_tokens() as f64
            })
        }
        TargetMetricKind::PostLandFailureRate => {
            measure_land_ratio(ledger, target_key, slice, |record| {
                f64::from(record.post_land_failures)
            })
        }
        TargetMetricKind::RevertRate => measure_land_ratio(ledger, target_key, slice, |record| {
            f64::from(record.reverts)
        }),
        TargetMetricKind::CheckQueueWaitMsMedian => {
            measure_check_wait_median(ledger, target_key, slice)
        }
        TargetMetricKind::RetriesPerIssue => measure_retries_per_issue(ledger, target_key, slice),
        TargetMetricKind::ReviewFindingsPerLand => {
            measure_land_ratio(ledger, target_key, slice, |record| {
                f64::from(record.review_findings)
            })
        }
    }
}

fn measure_idle_rate(ledger: &FleetLedger, slice: WindowSlice, period_secs: f64) -> MetricSample {
    let observed_secs = slice.observed_span_secs();
    if observed_secs == 0 {
        return MetricSample {
            value: None,
            samples: 0,
        };
    }
    let effective_start = slice.to_unix.saturating_sub(observed_secs);
    let mut idle_secs = 0.0;
    for (&hour_start, &bucket_secs) in &ledger.idle_slot_seconds_by_hour {
        let hour_end = hour_start.saturating_add(HOUR_SECONDS);
        let overlap_start = effective_start.max(hour_start);
        let overlap_end = slice.to_unix.min(hour_end);
        if overlap_end > overlap_start {
            let fraction = overlap_end.saturating_sub(overlap_start) as f64 / HOUR_SECONDS as f64;
            idle_secs += bucket_secs as f64 * fraction;
        }
    }
    let idle_minutes = idle_secs / 60.0;
    let rate = idle_minutes / (observed_secs as f64 / period_secs);
    let samples = (observed_secs / HOUR_SECONDS) as u32;
    MetricSample {
        value: Some(rate),
        samples,
    }
}

fn measure_land_rate(
    ledger: &FleetLedger,
    target_key: &str,
    slice: WindowSlice,
    period_secs: f64,
) -> MetricSample {
    let observed_secs = slice.observed_span_secs();
    let lands = ledger
        .issues
        .iter()
        .filter(|(key, record)| {
            key.as_str() != target_key && record.landed_at.is_some_and(|ts| slice.contains(ts))
        })
        .count() as u32;
    if observed_secs == 0 || lands == 0 {
        return MetricSample {
            value: (observed_secs > 0).then_some(0.0),
            samples: lands,
        };
    }
    let rate = f64::from(lands) / (observed_secs as f64 / period_secs);
    MetricSample {
        value: Some(rate),
        samples: lands,
    }
}

fn measure_land_median(
    ledger: &FleetLedger,
    target_key: &str,
    slice: WindowSlice,
    extract: impl Fn(&FleetIssueRecord) -> Option<u64>,
) -> MetricSample {
    let mut values: Vec<u64> = ledger
        .issues
        .iter()
        .filter(|(key, record)| {
            key.as_str() != target_key && record.landed_at.is_some_and(|ts| slice.contains(ts))
        })
        .filter_map(|(_, record)| extract(record))
        .collect();
    let samples = values.len() as u32;
    let value = median_u64(&mut values).map(|median| median as f64);
    MetricSample { value, samples }
}

fn measure_land_ratio(
    ledger: &FleetLedger,
    target_key: &str,
    slice: WindowSlice,
    numerator: impl Fn(&FleetIssueRecord) -> f64,
) -> MetricSample {
    let mut lands = 0u32;
    let mut total = 0.0;
    for (key, record) in &ledger.issues {
        if key.as_str() == target_key || !record.landed_at.is_some_and(|ts| slice.contains(ts)) {
            continue;
        }
        lands = lands.saturating_add(1);
        total += numerator(record);
    }
    MetricSample {
        value: (lands > 0).then(|| total / f64::from(lands)),
        samples: lands,
    }
}

fn measure_check_wait_median(
    ledger: &FleetLedger,
    target_key: &str,
    slice: WindowSlice,
) -> MetricSample {
    let mut waits: Vec<u64> = ledger
        .issues
        .iter()
        .filter(|(key, _)| key.as_str() != target_key)
        .flat_map(|(_, record)| record.checks.iter())
        .filter(|check| slice.contains(check.queued_at))
        .map(|check| check.wait_ms)
        .collect();
    let samples = waits.len() as u32;
    let value = median_u64(&mut waits).map(|median| median as f64);
    MetricSample { value, samples }
}

fn measure_retries_per_issue(
    ledger: &FleetLedger,
    target_key: &str,
    slice: WindowSlice,
) -> MetricSample {
    let mut issues = 0u32;
    let mut retries = 0u64;
    for (key, record) in &ledger.issues {
        if key.as_str() == target_key {
            continue;
        }
        let dispatch_ts = record.last_dispatched_at.or(record.dispatched_at);
        if dispatch_ts.is_some_and(|ts| slice.contains(ts)) {
            issues = issues.saturating_add(1);
            retries = retries.saturating_add(u64::from(record.retries));
        }
    }
    MetricSample {
        value: (issues > 0).then(|| retries as f64 / f64::from(issues)),
        samples: issues,
    }
}

fn earliest_ledger_timestamp(ledger: &FleetLedger) -> Option<u64> {
    let earliest_idle = ledger.idle_slot_seconds_by_hour.keys().next().copied();
    let earliest_issue = ledger
        .issues
        .values()
        .flat_map(|record| {
            [
                record.dispatched_at,
                record.last_dispatched_at,
                record.first_snapshot_at,
                record.named_at,
                record.landed_at,
            ]
            .into_iter()
            .flatten()
        })
        .min();
    match (earliest_idle, earliest_issue) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

fn classify_verdict(
    spec: ParsedTargetMetric,
    before: &MetricSample,
    after: &MetricSample,
) -> (ImprovementVerdict, Option<i32>) {
    let (Some(before_val), Some(after_val)) = (before.value, after.value) else {
        return (ImprovementVerdict::InsufficientSamples, None);
    };
    let relative = if before_val.abs() > RATIO_EPSILON {
        Some((after_val - before_val) / before_val)
    } else if after_val.abs() <= RATIO_EPSILON {
        Some(0.0)
    } else {
        None
    };
    let relative_bps = relative.map(|ratio| {
        let clamped = (ratio * 10_000.0).clamp(-1_000_000.0, 1_000_000.0);
        clamped.round() as i32
    });
    if before.samples < MIN_SAMPLES_PER_SIDE || after.samples < MIN_SAMPLES_PER_SIDE {
        return (ImprovementVerdict::InsufficientSamples, relative_bps);
    }
    let regressed = match (spec.direction, relative) {
        (TargetDirection::Down, Some(ratio)) => ratio + RATIO_EPSILON >= REGRESSION_THRESHOLD_RATIO,
        (TargetDirection::Up, Some(ratio)) => -ratio + RATIO_EPSILON >= REGRESSION_THRESHOLD_RATIO,
        (TargetDirection::Down, None) => after_val >= spec.kind.zero_baseline_regression_floor(),
        (TargetDirection::Up, None) => false,
    };
    if regressed {
        return (ImprovementVerdict::Regressed, relative_bps);
    }
    let improved = match (spec.direction, relative) {
        (TargetDirection::Down, Some(ratio)) => {
            -ratio + RATIO_EPSILON >= IMPROVEMENT_THRESHOLD_RATIO
        }
        (TargetDirection::Up, Some(ratio)) => ratio + RATIO_EPSILON >= IMPROVEMENT_THRESHOLD_RATIO,
        (TargetDirection::Up, None) => after_val > RATIO_EPSILON,
        (TargetDirection::Down, None) => false,
    };
    let verdict = if improved {
        ImprovementVerdict::Improved
    } else {
        ImprovementVerdict::NoSignificantChange
    };
    (verdict, relative_bps)
}

fn scale_value(value: f64) -> i64 {
    (value * METRIC_SCALE)
        .clamp((i64::MIN / 2) as f64, (i64::MAX / 2) as f64)
        .round() as i64
}

/// Stable HTML marker embedded in the outcome comment so retries never double-post.
pub fn outcome_comment_marker(outcome: &ImprovementOutcomeRecord) -> String {
    format!(
        "<!-- velocity-improvement-outcome:{}:{} -->",
        outcome.issue,
        outcome.land_token()
    )
}

/// Stable HTML marker embedded in a regression follow-up issue so duplicates are never filed.
pub fn regression_issue_marker(outcome: &ImprovementOutcomeRecord) -> String {
    format!(
        "<!-- velocity-improvement-regression:{}#{}:{} -->",
        outcome.repository.to_ascii_lowercase(),
        outcome.issue,
        outcome.land_token()
    )
}

/// Formats the Markdown outcome comment posted on the landed self-improvement issue (#621).
pub fn format_outcome_comment(outcome: &ImprovementOutcomeRecord) -> String {
    let metric_label = outcome
        .metric
        .map(TargetMetricKind::label)
        .unwrap_or("Unrecognized metric");
    let direction = outcome
        .direction
        .map(TargetDirection::description)
        .unwrap_or("unknown direction");
    let sample_unit = outcome
        .metric
        .map_or("samples", TargetMetricKind::sample_unit);
    let before_hours = duration_hours(outcome.before_from_unix, outcome.before_to_unix);
    let after_hours = duration_hours(outcome.after_from_unix, outcome.after_to_unix);
    let before_text = format_metric_value(outcome.metric, outcome.before_value());
    let after_text = format_metric_value(outcome.metric, outcome.after_value());
    let change_text = format_metric_change(outcome);
    let mut lines = vec![
        "## Self-improvement outcome".to_owned(),
        String::new(),
        format!(
            "- **Target metric:** `{}` ({metric_label}, {direction})",
            outcome.target_metric_raw
        ),
        format!("- **Verdict:** {}", outcome.verdict.label()),
        format!(
            "- **Before:** {before_text} (`n = {}` {sample_unit}, {} lands over {before_hours:.1}h)",
            outcome.before_samples, outcome.before_lands
        ),
        format!(
            "- **After:** {after_text} (`n = {}` {sample_unit}, {} lands over {after_hours:.1}h)",
            outcome.after_samples, outcome.after_lands
        ),
        format!("- **Change:** {change_text}"),
    ];
    if let Some(commit) = outcome.landed_commit.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("- **Landed commit:** `{commit}`"));
    }
    if let Some(follow_up) = outcome.regression_issue {
        lines.push(format!("- **Regression follow-up:** #{follow_up}"));
    }
    lines.push(String::new());
    lines.push(outcome_comment_marker(outcome));
    lines.join("\n")
}

/// Builds a follow-up `self-improvement` issue draft when `outcome.verdict == Regressed`.
pub fn build_regression_followup_draft(
    outcome: &ImprovementOutcomeRecord,
) -> Option<RegressionFollowUpDraft> {
    if outcome.verdict != ImprovementVerdict::Regressed {
        return None;
    }
    let metric = outcome.metric?;
    let direction = outcome
        .direction
        .unwrap_or_else(|| metric.natural_direction());
    let before_text = format_metric_value(Some(metric), outcome.before_value());
    let after_text = format_metric_value(Some(metric), outcome.after_value());
    let change_text = format_metric_change(outcome);
    let (commit_ref, change_suffix, source_link) = format_land_references(outcome);
    let title = preview(
        &format!(
            "Regression after #{}: {} worsened ({change_text})",
            outcome.issue,
            metric.label()
        ),
        240,
    );
    let marker = regression_issue_marker(outcome);
    let body = format!(
        "Landed change for {source_link} (commit {commit_ref}{change_suffix}) regressed its target metric once its post-land window closed.\n\n\
         Target metric: {}\n\n\
         ## Measured regression\n\n\
         - **Metric:** {} ({})\n\
         - **Before:** {before_text} (`n = {}` {}, {} lands over {:.1}h)\n\
         - **After:** {after_text} (`n = {}` {}, {} lands over {:.1}h)\n\
         - **Change:** {change_text} (regression threshold: {}% worse with at least {MIN_SAMPLES_PER_SIDE} samples per side)\n\
         - **Landed commit:** {commit_ref}\n\n\
         ## Next step\n\n\
         Investigate why {source_link} worsened {} and decide whether to fix it forward or revert {commit_ref}. Velocity did not revert automatically.\n\n\
         {marker}\n",
        outcome.target_metric_raw,
        metric.label(),
        direction.description(),
        outcome.before_samples,
        metric.sample_unit(),
        outcome.before_lands,
        duration_hours(outcome.before_from_unix, outcome.before_to_unix),
        outcome.after_samples,
        metric.sample_unit(),
        outcome.after_lands,
        duration_hours(outcome.after_from_unix, outcome.after_to_unix),
        (REGRESSION_THRESHOLD_RATIO * 100.0).round() as u32,
        metric.label(),
    );
    Some(RegressionFollowUpDraft {
        title,
        body,
        label: SELF_IMPROVEMENT_LABEL.to_owned(),
        marker,
    })
}

fn format_land_references(outcome: &ImprovementOutcomeRecord) -> (String, String, String) {
    let commit_ref = outcome
        .landed_commit
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|sha| format!("`{sha}`"))
        .unwrap_or_else(|| format!("land at unix {}", outcome.landed_at));
    let change_suffix = outcome
        .change
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|change| format!(", Rosen change `{change}`"))
        .unwrap_or_default();
    let source_link = outcome
        .url
        .as_deref()
        .filter(|url| !url.trim().is_empty())
        .map(|url| format!("#{} ({url})", outcome.issue))
        .unwrap_or_else(|| format!("#{}", outcome.issue));
    (commit_ref, change_suffix, source_link)
}

/// Formats one compact digest line for the improvement scout (#622).
pub fn format_outcome_digest_line(
    project_name: &str,
    outcome: &ImprovementOutcomeRecord,
) -> String {
    let metric = outcome
        .metric
        .map(TargetMetricKind::label)
        .unwrap_or(outcome.target_metric_raw.as_str());
    let before = format_metric_value(outcome.metric, outcome.before_value());
    let after = format_metric_value(outcome.metric, outcome.after_value());
    let change = format_metric_change(outcome);
    let follow_up = outcome
        .regression_issue
        .map(|number| format!(" · follow-up #{number}"))
        .unwrap_or_default();
    format!(
        "- [{project_name} · {}#{}] {}: {} ({before} -> {after}, {change}, n={}/{}){follow_up}",
        outcome.repository,
        outcome.issue,
        outcome.verdict.label(),
        preview(metric, 80),
        outcome.before_samples,
        outcome.after_samples,
    )
}

/// Builds the list of recent improvements (pending and measured) for the Fleet status pane (#621).
pub fn build_recent_improvement_views(
    ledger: &FleetLedger,
    now_secs: u64,
) -> Vec<FleetImprovementOutcomeView> {
    let mut rows: Vec<FleetImprovementOutcomeView> = ledger
        .improvement_outcomes
        .iter()
        .map(FleetImprovementOutcomeView::from)
        .collect();
    for record in ledger.issues.values() {
        if let Some(pending) = build_pending_improvement_view(ledger, record, now_secs) {
            rows.push(pending);
        }
    }
    rows.sort_by(|left, right| {
        right
            .landed_at_secs
            .total_cmp(&left.landed_at_secs)
            .then_with(|| right.issue.cmp(&left.issue))
    });
    rows.truncate(RECENT_IMPROVEMENTS_LIMIT);
    rows
}

fn build_pending_improvement_view(
    ledger: &FleetLedger,
    record: &FleetIssueRecord,
    now_secs: u64,
) -> Option<FleetImprovementOutcomeView> {
    let landed_at = record.landed_at?;
    let raw_metric = record
        .target_metric
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let already_measured = ledger
        .improvement_outcomes
        .iter()
        .any(|outcome| outcome.matches_land(&record.repository, record.issue, landed_at));
    if already_measured {
        return None;
    }
    let target_key = issue_key(&record.repository, record.issue);
    let (_, after_lands) = partition_other_lands(ledger, &target_key, landed_at, now_secs);
    let elapsed_hours = now_secs.saturating_sub(landed_at) as f64 / HOUR_SECONDS as f64;
    let parsed = parse_target_metric_spec(raw_metric);
    Some(FleetImprovementOutcomeView {
        repository: record.repository.clone(),
        issue: record.issue,
        title: record.title.clone(),
        url: record.url.clone(),
        target_metric: raw_metric.to_owned(),
        metric_label: parsed
            .map(|spec| spec.kind.label().to_owned())
            .unwrap_or_else(|| raw_metric.to_owned()),
        verdict: ImprovementVerdict::NoSignificantChange,
        before_text: "—".to_owned(),
        after_text: "—".to_owned(),
        change_text: "Measuring".to_owned(),
        before_samples: 0,
        after_samples: after_lands.len() as u32,
        landed_at_secs: landed_at as f64,
        measured_at_secs: 0.0,
        regression_issue: None,
        regression_issue_url: None,
        pending: true,
        pending_summary: Some(format!(
            "{}/{} lands · {:.0}/{}h",
            after_lands.len().min(OUTCOME_WINDOW_MIN_LANDS),
            OUTCOME_WINDOW_MIN_LANDS,
            elapsed_hours.min(OUTCOME_WINDOW_MIN_HOURS as f64),
            OUTCOME_WINDOW_MIN_HOURS,
        )),
    })
}

impl From<&ImprovementOutcomeRecord> for FleetImprovementOutcomeView {
    fn from(outcome: &ImprovementOutcomeRecord) -> Self {
        Self {
            repository: outcome.repository.clone(),
            issue: outcome.issue,
            title: outcome.title.clone(),
            url: outcome.url.clone(),
            target_metric: outcome.target_metric_raw.clone(),
            metric_label: outcome
                .metric
                .map(|kind| kind.label().to_owned())
                .unwrap_or_else(|| outcome.target_metric_raw.clone()),
            verdict: outcome.verdict,
            before_text: format_metric_value(outcome.metric, outcome.before_value()),
            after_text: format_metric_value(outcome.metric, outcome.after_value()),
            change_text: format_metric_change(outcome),
            before_samples: outcome.before_samples,
            after_samples: outcome.after_samples,
            landed_at_secs: outcome.landed_at as f64,
            measured_at_secs: outcome.measured_at as f64,
            regression_issue: outcome.regression_issue,
            regression_issue_url: outcome.regression_issue_url.clone(),
            pending: false,
            pending_summary: None,
        }
    }
}

pub fn format_metric_value(kind: Option<TargetMetricKind>, value: Option<f64>) -> String {
    let Some(value) = value else {
        return "—".to_owned();
    };
    match kind {
        Some(TargetMetricKind::IdleSlotMinutesPerDay) => format!("{value:.1} slot-min/day"),
        Some(TargetMetricKind::IdleSlotMinutesPerHour) => format!("{value:.1} slot-min/hr"),
        Some(TargetMetricKind::LandsPerHour) => format!("{value:.2} lands/hr"),
        Some(TargetMetricKind::LandsPerDay) => format!("{value:.1} lands/day"),
        Some(
            TargetMetricKind::MedianDispatchedToLandedSecs
            | TargetMetricKind::MedianFiledToLandedSecs,
        ) => format_seconds(value),
        Some(TargetMetricKind::TokensPerLandedChange) => format_tokens(value),
        Some(TargetMetricKind::PostLandFailureRate | TargetMetricKind::RevertRate) => {
            format!("{:.1}%", value * 100.0)
        }
        Some(TargetMetricKind::CheckQueueWaitMsMedian) => format_millis(value),
        Some(TargetMetricKind::RetriesPerIssue) => format!("{value:.2} retries/issue"),
        Some(TargetMetricKind::ReviewFindingsPerLand) => format!("{value:.2} findings/land"),
        None => format!("{value:.2}"),
    }
}

pub fn format_metric_change(outcome: &ImprovementOutcomeRecord) -> String {
    let (Some(before), Some(after)) = (outcome.before_value(), outcome.after_value()) else {
        return "—".to_owned();
    };
    let delta = after - before;
    let sign = if delta > 0.0 { "+" } else { "" };
    let pct_suffix = outcome
        .relative_change_pct()
        .map(|pct| {
            let pct_sign = if pct > 0.0 { "+" } else { "" };
            format!(" ({pct_sign}{pct:.1}%)")
        })
        .unwrap_or_default();
    match outcome.metric {
        Some(TargetMetricKind::PostLandFailureRate | TargetMetricKind::RevertRate) => {
            format!("{sign}{:.1}pp{pct_suffix}", delta * 100.0)
        }
        Some(
            TargetMetricKind::MedianDispatchedToLandedSecs
            | TargetMetricKind::MedianFiledToLandedSecs,
        ) => {
            let abs_text = format_seconds(delta.abs());
            let signed = if delta < 0.0 {
                format!("-{abs_text}")
            } else {
                format!("{sign}{abs_text}")
            };
            format!("{signed}{pct_suffix}")
        }
        Some(TargetMetricKind::TokensPerLandedChange) => {
            let abs_text = format_tokens(delta.abs());
            let signed = if delta < 0.0 {
                format!("-{abs_text}")
            } else {
                format!("{sign}{abs_text}")
            };
            format!("{signed}{pct_suffix}")
        }
        Some(TargetMetricKind::CheckQueueWaitMsMedian) => {
            let abs_text = format_millis(delta.abs());
            let signed = if delta < 0.0 {
                format!("-{abs_text}")
            } else {
                format!("{sign}{abs_text}")
            };
            format!("{signed}{pct_suffix}")
        }
        Some(
            TargetMetricKind::LandsPerHour
            | TargetMetricKind::RetriesPerIssue
            | TargetMetricKind::ReviewFindingsPerLand,
        ) => {
            format!("{sign}{delta:.2}{pct_suffix}")
        }
        _ => format!("{sign}{delta:.1}{pct_suffix}"),
    }
}

fn duration_hours(from_unix: u64, to_unix: u64) -> f64 {
    to_unix.saturating_sub(from_unix) as f64 / HOUR_SECONDS as f64
}

fn format_seconds(seconds: f64) -> String {
    let rounded = seconds.max(0.0).round() as u64;
    if rounded < 60 {
        return format!("{rounded}s");
    }
    let minutes = rounded / 60;
    if minutes < 60 {
        let rem = rounded % 60;
        return if rem > 0 {
            format!("{minutes}m {rem}s")
        } else {
            format!("{minutes}m")
        };
    }
    let hours = minutes / 60;
    let rem_minutes = minutes % 60;
    if rem_minutes > 0 {
        format!("{hours}h {rem_minutes}m")
    } else {
        format!("{hours}h")
    }
}

fn format_millis(ms: f64) -> String {
    if ms < 1_000.0 {
        format!("{}ms", ms.round() as u64)
    } else if ms < 60_000.0 {
        format!("{:.1}s", ms / 1_000.0)
    } else {
        format_seconds(ms / 1_000.0)
    }
}

fn format_tokens(tokens: f64) -> String {
    if tokens >= 1_000_000.0 {
        format!("{:.1}M tokens", tokens / 1_000_000.0)
    } else if tokens >= 1_000.0 {
        format!("{:.0}k tokens", tokens / 1_000.0)
    } else {
        format!("{} tokens", tokens.round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet_ledger::FleetCheckRecord;

    fn add_landed_issue(
        ledger: &mut FleetLedger,
        number: u32,
        landed_at: u64,
        dispatch_duration_secs: u64,
        tokens: u64,
    ) {
        let mut record = FleetIssueRecord::new("mESmaC/velocity".into(), number);
        record.dispatched_at = Some(landed_at.saturating_sub(dispatch_duration_secs));
        record.landed_at = Some(landed_at);
        record.tokens_in = tokens;
        ledger
            .issues
            .insert(issue_key("mESmaC/velocity", number), record);
    }

    // Risk (#621): the post-land window must wait for BOTH 24 hours and 20 lands
    // (whichever is longer), then accurately compare before vs after and format the comment.
    #[test]
    fn window_waits_for_longer_of_24h_or_20_lands_and_measures_improvement()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut ledger = FleetLedger::default();
        let land_ts = 1_795_400_000;

        // 20 lands before the improvement, each taking 1,000s from dispatch to land.
        for index in 0..20u32 {
            add_landed_issue(
                &mut ledger,
                100 + index,
                land_ts - 30 * HOUR_SECONDS + u64::from(index) * HOUR_SECONDS,
                1_000,
                100_000,
            );
        }

        // The self-improvement issue itself.
        let mut target = FleetIssueRecord::new("mESmaC/velocity".into(), 621);
        target.title = Some("Speed up preflight".into());
        target.url = Some("https://collapse.sh/mESmaC/velocity/issues/621".into());
        target.landed_at = Some(land_ts);
        target.landed_commit = Some("abcdef1234567890".into());
        target.target_metric = Some("median dispatch -> land time, down".into());
        ledger
            .issues
            .insert(issue_key("mESmaC/velocity", 621), target.clone());

        // 19 lands after over 30 hours -> time passed (> 24h), but < 20 lands, so still open.
        for index in 0..19u32 {
            add_landed_issue(
                &mut ledger,
                200 + index,
                land_ts + HOUR_SECONDS + u64::from(index) * HOUR_SECONDS,
                600,
                80_000,
            );
        }
        assert!(evaluate_issue_outcome(&ledger, &target, land_ts + 30 * HOUR_SECONDS).is_none());

        // 20th land arrives at +32 hours -> window closes at +32h (20 lands was longer than 24h).
        add_landed_issue(&mut ledger, 219, land_ts + 32 * HOUR_SECONDS, 600, 80_000);
        // Still closed only once `now_secs >= land_ts + 24h` and 20 lands exist.
        assert!(evaluate_issue_outcome(&ledger, &target, land_ts + 20 * HOUR_SECONDS).is_none());

        let outcome = evaluate_issue_outcome(&ledger, &target, land_ts + 33 * HOUR_SECONDS)
            .ok_or("expected closed window outcome")?;
        assert_eq!(outcome.verdict, ImprovementVerdict::Improved);
        assert_eq!(outcome.before_samples, 20);
        assert_eq!(outcome.after_samples, 20);
        assert_eq!(outcome.before_value(), Some(1_000.0));
        assert_eq!(outcome.after_value(), Some(600.0));
        assert_eq!(outcome.relative_change_bps, Some(-4_000));
        assert_eq!(outcome.after_to_unix, land_ts + 32 * HOUR_SECONDS);
        assert!(build_regression_followup_draft(&outcome).is_none());

        let comment = format_outcome_comment(&outcome);
        assert!(comment.contains("Improved"));
        assert!(comment.contains("n = 20"));
        assert!(comment.contains("-40.0%"));
        assert!(comment.contains("<!-- velocity-improvement-outcome:621:abcdef123456 -->"));
        Ok(())
    }

    // Risk (#621): a 25% regression with >= 10 samples on each side must trigger a
    // `self-improvement` follow-up draft linking the land, while smaller regressions or
    // insufficient samples (< 10 per side) must NOT trigger a follow-up.
    #[test]
    fn regression_thresholds_and_followup_draft_enforce_25pct_and_10_samples()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut ledger = FleetLedger::default();
        let land_ts = 1_795_500_000;

        // Only 9 lands before -> below the 10-sample minimum.
        for index in 0..9u32 {
            add_landed_issue(
                &mut ledger,
                300 + index,
                land_ts - 10 * HOUR_SECONDS + u64::from(index) * HOUR_SECONDS,
                1_000,
                100_000,
            );
        }
        let mut target = FleetIssueRecord::new("mESmaC/velocity".into(), 700);
        target.url = Some("https://collapse.sh/mESmaC/velocity/issues/700".into());
        target.change = Some("chg-700".into());
        target.landed_commit = Some("deadbeef1234".into());
        target.landed_at = Some(land_ts);
        target.target_metric = Some("tokens per landed change, down".into());
        ledger
            .issues
            .insert(issue_key("mESmaC/velocity", 700), target.clone());

        // 20 lands after at 130,000 tokens (+30% worse), all within 24h.
        for index in 0..20u32 {
            add_landed_issue(
                &mut ledger,
                400 + index,
                land_ts + 1_000 + u64::from(index) * 1_000,
                1_000,
                130_000,
            );
        }

        // With 9 before samples, verdict is InsufficientSamples -> NO regression follow-up.
        let low_n = evaluate_issue_outcome(&ledger, &target, land_ts + OUTCOME_WINDOW_MIN_SECONDS)
            .ok_or("expected outcome with 9 before samples")?;
        assert_eq!(low_n.verdict, ImprovementVerdict::InsufficientSamples);
        assert_eq!(low_n.before_samples, 9);
        assert_eq!(low_n.after_samples, 20);
        assert!(build_regression_followup_draft(&low_n).is_none());

        // Add a 10th before sample -> now 10 before, 20 after, +30% worse -> Regressed!
        add_landed_issue(
            &mut ledger,
            309,
            land_ts - 11 * HOUR_SECONDS,
            1_000,
            100_000,
        );
        let regressed =
            evaluate_issue_outcome(&ledger, &target, land_ts + OUTCOME_WINDOW_MIN_SECONDS)
                .ok_or("expected regressed outcome")?;
        assert_eq!(regressed.verdict, ImprovementVerdict::Regressed);
        assert_eq!(regressed.before_samples, 10);
        assert_eq!(regressed.after_samples, 20);
        assert_eq!(regressed.relative_change_bps, Some(3_000));

        let draft = build_regression_followup_draft(&regressed)
            .ok_or("expected regression follow-up issue draft")?;
        assert_eq!(draft.label, SELF_IMPROVEMENT_LABEL);
        assert!(draft.title.contains("#700"));
        assert!(draft.body.contains("deadbeef1234"));
        assert!(draft.body.contains("chg-700"));
        assert!(draft.body.contains("did not revert automatically"));
        assert!(
            draft
                .body
                .contains("Target metric: tokens per landed change, down")
        );
        assert!(
            draft.body.contains(
                "<!-- velocity-improvement-regression:mesmac/velocity#700:deadbeef1234 -->"
            )
        );

        // A 20% worsening (below 25%) must NOT be classified as Regressed.
        for index in 0..20u32 {
            if let Some(entry) = ledger
                .issues
                .get_mut(&issue_key("mESmaC/velocity", 400 + index))
            {
                entry.tokens_in = 120_000;
            }
        }
        let mild = evaluate_issue_outcome(&ledger, &target, land_ts + OUTCOME_WINDOW_MIN_SECONDS)
            .ok_or("expected mild outcome")?;
        assert_eq!(mild.verdict, ImprovementVerdict::NoSignificantChange);
        assert!(build_regression_followup_draft(&mild).is_none());
        Ok(())
    }

    // Risk (#621): hourly idle slot-minutes and check queue wait metrics must normalize
    // across window boundaries and support both `down` and `up` target directions.
    #[test]
    fn parses_all_fleet_metrics_and_measures_idle_and_check_wait()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            parse_target_metric_spec("idle slot-minutes per day, down"),
            Some(ParsedTargetMetric {
                kind: TargetMetricKind::IdleSlotMinutesPerDay,
                direction: TargetDirection::Down,
            })
        );
        assert_eq!(
            parse_target_metric_spec("lands per hour, up"),
            Some(ParsedTargetMetric {
                kind: TargetMetricKind::LandsPerHour,
                direction: TargetDirection::Up,
            })
        );
        assert_eq!(
            parse_target_metric_spec("check queue wait, lower"),
            Some(ParsedTargetMetric {
                kind: TargetMetricKind::CheckQueueWaitMsMedian,
                direction: TargetDirection::Down,
            })
        );

        let mut ledger = FleetLedger::default();
        let land_ts = 1_795_600_800; // exact UTC hour
        // 24 hours before: 600 slot-secs/hr = 10 slot-min/hr = 240 slot-min/day.
        for hour in 0..24u64 {
            let start = land_ts - (24 - hour) * HOUR_SECONDS;
            ledger.record_idle_interval(start, start + 600, 1);
        }
        // 24 hours after: 900 slot-secs/hr = 15 slot-min/hr = 360 slot-min/day (+50% worse).
        for hour in 0..24u64 {
            let start = land_ts + hour * HOUR_SECONDS;
            ledger.record_idle_interval(start, start + 900, 1);
        }
        let mut target = FleetIssueRecord::new("mESmaC/velocity".into(), 701);
        target.landed_at = Some(land_ts);
        target.target_metric = Some("idle slot-minutes per day, down".into());
        ledger
            .issues
            .insert(issue_key("mESmaC/velocity", 701), target.clone());

        for index in 0..20u32 {
            add_landed_issue(
                &mut ledger,
                500 + index,
                land_ts + 60 + u64::from(index) * 60,
                500,
                50_000,
            );
            if let Some(entry) = ledger
                .issues
                .get_mut(&issue_key("mESmaC/velocity", 500 + index))
            {
                entry.checks.push(FleetCheckRecord {
                    id: format!("chk-{index}"),
                    command: "preflight".into(),
                    queued_at: land_ts + 60 + u64::from(index) * 60,
                    wait_ms: 2_000,
                    run_ms: 5_000,
                    succeeded: true,
                });
            }
        }

        let outcome =
            evaluate_issue_outcome(&ledger, &target, land_ts + OUTCOME_WINDOW_MIN_SECONDS)
                .ok_or("expected idle slot-minutes outcome")?;
        assert_eq!(outcome.verdict, ImprovementVerdict::Regressed);
        assert_eq!(outcome.before_samples, 24);
        assert_eq!(outcome.after_samples, 24);
        assert_eq!(outcome.before_value(), Some(240.0));
        assert_eq!(outcome.after_value(), Some(360.0));
        assert_eq!(outcome.relative_change_bps, Some(5_000));
        Ok(())
    }
}
