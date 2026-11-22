//! Per-project health checks and automatic remediation summaries (#509).
//!
//! Pure types, thresholds, and text extractors live here; Rosen reads, GitHub queries,
//! and remediation actions live in `velocity-rosen`, `velocity-threads`, and `src-tauri`.

use serde::{Deserialize, Serialize};

use crate::base_ci::BaseCiSource;
use crate::collapse::IssueHost;

/// Stable project-health key for non-owner issue host or CI source changes (#613).
pub const HOST_SOURCE_CHANGED_KEY: &str = "host-source-changed";

/// Stable project-health key for idle continuous-dispatch slots (#509, #616).
pub const DISPATCH_IDLE_KEY: &str = "dispatch-idle";

/// How long the mirror may sit behind the base before Velocity retries the push (#509).
pub const MIRROR_BEHIND_LIMIT_SECONDS: u64 = 10 * 60;

/// How many uncovered lands trigger an integration build health check (#509).
pub const INTEGRATION_UNCOVERED_LANDS: u32 = 10;

/// How long an uncovered base triggers an integration build health check (#509).
pub const INTEGRATION_UNCOVERED_SECONDS: u64 = 2 * 60 * 60;

/// How long landed changes may wait with no post-land verification result before
/// project health surfaces a stall (#567).
pub const POST_LAND_STALL_LIMIT_SECONDS: u64 = 60 * 60;

/// How long a named thread may sit in Review without landing before asking Rosen (#509).
pub const REVIEW_STUCK_LIMIT_SECONDS: u64 = 20 * 60;

/// How long free continuous-dispatch slots with open issues may stay idle before logging
/// why each issue was skipped to `velocity.log` (#509).
pub const DISPATCH_IDLE_LIMIT_SECONDS: u64 = 10 * 60;

/// How long free continuous-dispatch slots with nothing dispatched may stay idle before
/// surfacing in the title-bar health area, desktop notification, and health history (#616).
pub const DISPATCH_IDLE_ALERT_SECONDS: u64 = 30 * 60;

/// Minimum interval between in-flight duration refreshes written to project health history (#616).
pub const DISPATCH_IDLE_HISTORY_STEP_SECONDS: u64 = 5 * 60;

/// Maximum number of persisted project health history entries kept per project (#616).
pub const HEALTH_HISTORY_LIMIT: usize = 50;

/// How long a recorded non-GitHub AIRC issue tool failure stays on the project health strip
/// if no subsequent issue tool call succeeds to clear it (#581).
pub const AIRC_ISSUE_FAILURE_TTL_SECONDS: u64 = 2 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum HealthSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum HealthSignalKind {
    MirrorBehind,
    CollapseProactiveAuth,
    CollapseForwarding,
    CollapseIssuesNotImported,
    HostSourceChanged,
    HostIssueMissing,
    HostIssueMismatch,
    ReopenedAlreadyLanded,
    IntegrationUncovered,
    BaseRed,
    LandedIssuesWaiting,
    IssueParked,
    ReviewStalled,
    DispatchIdle,
    PostLandStopped,
    PostLandStalled,
    UpdaterManifestBehind,
    ReleaseGateRefused,
    AircIssueToolFailed,
    Deploy,
    LocalBuildRestartFailed,
}

/// Persisted record of the latest non-GitHub AIRC issue tool failure for one project (#581).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AircIssueToolFailure {
    pub tool: String,
    pub message: String,
    pub count: u32,
    pub failed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_identifier: Option<u32>,
}

/// Formats a non-GitHub AIRC issue tool failure as a project health row (#581).
pub fn airc_issue_tool_failure_item(failure: &AircIssueToolFailure) -> ProjectHealthItem {
    let signal = if failure.count <= 1 {
        "AIRC issue tool failed locally".to_owned()
    } else {
        format!("AIRC issue tools failed locally ({}x)", failure.count)
    };
    let action = format!("`{}` failed before reaching GitHub", failure.tool);
    ProjectHealthItem::new(
        "airc-issue-tool-failed",
        HealthSignalKind::AircIssueToolFailed,
        HealthSeverity::Error,
        signal,
        action,
        failure.failed_at_ms,
    )
    .with_detail(Some(failure.message.clone()))
    .with_thread(failure.thread_identifier)
}

/// One-click revert metadata for an unexpected issue host or CI source change (#613).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct HostSwitchBack {
    /// Label for the one-click revert button (for example `Switch back to Collapse`).
    pub button_label: String,
    /// Label for acknowledging the current host/CI setting without reverting.
    pub keep_label: String,
    /// Whether switching back can be performed directly in one click.
    pub can_switch_back: bool,
}

/// Persisted record of a non-owner change to a project's effective issue host or base CI
/// source, shown in project health until the owner switches back or keeps it (#613).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSourceChange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_issue_host: Option<IssueHost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_issue_host: Option<IssueHost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_ci_source: Option<BaseCiSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_ci_source: Option<BaseCiSource>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub detected_at_ms: u64,
}

impl HostSourceChange {
    pub fn issue_host_changed(&self) -> Option<(IssueHost, IssueHost)> {
        match (self.previous_issue_host, self.current_issue_host) {
            (Some(previous), Some(current)) if previous != current => Some((previous, current)),
            _ => None,
        }
    }

    pub fn ci_source_changed(&self) -> Option<(BaseCiSource, BaseCiSource)> {
        match (self.previous_ci_source, self.current_ci_source) {
            (Some(previous), Some(current)) if previous != current => Some((previous, current)),
            _ => None,
        }
    }

    pub fn has_change(&self) -> bool {
        self.issue_host_changed().is_some() || self.ci_source_changed().is_some()
    }
}

/// Whether `change` can be switched back directly in one click without violating project
/// invariants (a Collapse issue host requires a Collapse repository, and a Collapse-mirrored
/// project never follows GitHub Actions for base CI) (#613).
pub fn host_source_change_can_switch_back(
    change: &HostSourceChange,
    has_collapse_repository: bool,
    has_github_remote: bool,
) -> bool {
    let host_ok = match change.issue_host_changed() {
        Some((IssueHost::Collapse, _)) => has_collapse_repository,
        Some((IssueHost::GitHub, _)) | None => true,
    };
    let ci_ok = match change.ci_source_changed() {
        Some((BaseCiSource::GitHub, _)) => !has_collapse_repository && has_github_remote,
        Some((BaseCiSource::Rosen, _)) | None => true,
    };
    change.has_change() && host_ok && ci_ok
}

/// Formats a non-owner issue host or base CI source change as a project health row (#613).
pub fn host_source_change_item(
    change: &HostSourceChange,
    has_collapse_repository: bool,
    has_github_remote: bool,
) -> Option<ProjectHealthItem> {
    let host_change = change.issue_host_changed();
    let ci_change = change.ci_source_changed();
    if host_change.is_none() && ci_change.is_none() {
        return None;
    }
    let can_switch_back =
        host_source_change_can_switch_back(change, has_collapse_repository, has_github_remote);
    let (signal, button_label, keep_label) = host_change_labels(host_change, ci_change);
    let action = if can_switch_back {
        format!("Changed without an owner choice; click {button_label} to revert")
    } else if ci_change == Some((BaseCiSource::GitHub, BaseCiSource::Rosen))
        && has_collapse_repository
    {
        "Collapse-mirrored projects always use Rosen checkpoints instead of GitHub Actions"
            .to_owned()
    } else {
        "Changed without an owner choice; re-link the previous repository to switch back".to_owned()
    };
    let detail = host_change_detail(host_change, ci_change, &change.reason);
    Some(
        ProjectHealthItem::new(
            HOST_SOURCE_CHANGED_KEY,
            HealthSignalKind::HostSourceChanged,
            HealthSeverity::Warning,
            signal,
            action,
            change.detected_at_ms,
        )
        .with_detail(detail)
        .with_host_switch_back(Some(HostSwitchBack {
            button_label,
            keep_label,
            can_switch_back,
        })),
    )
}

fn host_change_labels(
    host_change: Option<(IssueHost, IssueHost)>,
    ci_change: Option<(BaseCiSource, BaseCiSource)>,
) -> (String, String, String) {
    match (host_change, ci_change) {
        (Some((prev, curr)), None) => (
            format!(
                "Issue host changed from {} to {}",
                issue_host_label(prev),
                issue_host_label(curr)
            ),
            format!("Switch back to {}", issue_host_label(prev)),
            format!("Keep {}", issue_host_label(curr)),
        ),
        (None, Some((prev, curr))) => (
            format!(
                "Base CI changed from {} to {}",
                ci_source_label(prev),
                ci_source_label(curr)
            ),
            format!("Switch back to {} CI", ci_short_label(prev)),
            format!("Keep {} CI", ci_short_label(curr)),
        ),
        (Some((prev_host, curr_host)), Some((prev_ci, curr_ci))) => (
            format!(
                "Issue host and base CI changed ({} / {} → {} / {})",
                issue_host_label(prev_host),
                ci_short_label(prev_ci),
                issue_host_label(curr_host),
                ci_short_label(curr_ci),
            ),
            format!(
                "Switch back to {} & {}",
                issue_host_label(prev_host),
                ci_short_label(prev_ci)
            ),
            format!(
                "Keep {} & {}",
                issue_host_label(curr_host),
                ci_short_label(curr_ci)
            ),
        ),
        (None, None) => (String::new(), String::new(), String::new()),
    }
}

fn host_change_detail(
    host_change: Option<(IssueHost, IssueHost)>,
    ci_change: Option<(BaseCiSource, BaseCiSource)>,
    reason: &str,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some((prev, curr)) = host_change {
        parts.push(format!(
            "Issues: {} → {}",
            issue_host_label(prev),
            issue_host_label(curr)
        ));
    }
    if let Some((prev, curr)) = ci_change {
        parts.push(format!(
            "CI: {} → {}",
            ci_source_label(prev),
            ci_source_label(curr)
        ));
    }
    let trimmed = reason.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_owned());
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn issue_host_label(host: IssueHost) -> &'static str {
    match host {
        IssueHost::GitHub => "GitHub",
        IssueHost::Collapse => "Collapse",
    }
}

fn ci_source_label(source: BaseCiSource) -> &'static str {
    match source {
        BaseCiSource::GitHub => "GitHub Actions",
        BaseCiSource::Rosen => "Rosen checkpoints",
    }
}

fn ci_short_label(source: BaseCiSource) -> &'static str {
    match source {
        BaseCiSource::GitHub => "GitHub",
        BaseCiSource::Rosen => "Rosen",
    }
}

/// Stable project-health key for a missing or mismatched host issue (#614).
pub fn host_issue_problem_key(number: u32) -> String {
    format!("host-issue:{number}")
}

/// Stable project-health key for a reopened issue whose fix already landed (#615).
pub fn reopened_already_landed_key(number: u32) -> String {
    format!("reopened-landed:{number}")
}

/// Inputs for formatting a reopened-already-landed project health row (#615).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReopenedLandedNotice<'a> {
    pub issue_number: u32,
    pub commit: &'a str,
    pub land_summary: &'a str,
    pub issue_title: Option<&'a str>,
    pub issue_url: Option<&'a str>,
    pub reopened_by_import: bool,
    pub detected_at_ms: u64,
}

/// Formats a reopened issue whose fix is already in Rosen land history as a project health row (#615).
pub fn reopened_already_landed_item(notice: ReopenedLandedNotice<'_>) -> ProjectHealthItem {
    let short = short_commit(notice.commit);
    let signal = format!(
        "#{} reopened, already landed in {short}",
        notice.issue_number
    );
    let action = if notice.reopened_by_import {
        "Reopened by import · held from dispatch (close on Collapse or edit to re-dispatch)"
            .to_owned()
    } else {
        "Held from dispatch · land history already contains a fix after the last reopen".to_owned()
    };
    let mut detail_parts = Vec::new();
    let land_summary = notice.land_summary.trim();
    if !land_summary.is_empty() {
        detail_parts.push(land_summary.to_owned());
    }
    if let Some(title) = notice
        .issue_title
        .map(str::trim)
        .filter(|title| !title.is_empty() && !title.eq_ignore_ascii_case(land_summary))
    {
        detail_parts.push(title.to_owned());
    }
    let mut item = ProjectHealthItem::new(
        reopened_already_landed_key(notice.issue_number),
        HealthSignalKind::ReopenedAlreadyLanded,
        HealthSeverity::Warning,
        signal,
        action,
        notice.detected_at_ms,
    )
    .with_detail((!detail_parts.is_empty()).then(|| detail_parts.join(" · ")));
    if let Some(url) = notice
        .issue_url
        .filter(|value| value.starts_with("https://"))
    {
        item = item.with_action_link(
            Some(url.to_owned()),
            format!("Open #{}", notice.issue_number),
        );
    }
    item
}

/// Optional remediation metadata for `HostIssueMissing` and `HostIssueMismatch` signals (#614).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct HostIssueAction {
    pub issue_number: u32,
    pub can_copy_from_github: bool,
    pub can_dismiss: bool,
}

/// Why Velocity stopped updating or matching a host issue (#614).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostIssueProblemKind {
    /// The issue returned HTTP 404 on the configured issue host; stop timer retries.
    Missing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        github_repository: Option<String>,
        #[serde(default)]
        github_exists: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        github_title: Option<String>,
    },
    /// The host issue at this number does not match the landed change's title or GitHub source.
    Mismatch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_source: Option<String>,
        #[serde(default)]
        host_title: String,
        #[serde(default)]
        host_url: String,
    },
}

/// Persisted per-issue permanent host failure (404 missing or number-collision mismatch) (#614).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostIssueProblem {
    pub number: u32,
    pub host: IssueHost,
    #[serde(default)]
    pub repository: String,
    pub kind: HostIssueProblemKind,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub detected_at_ms: u64,
}

/// Formats a missing or mismatched host issue as a project health row (#614).
pub fn host_issue_problem_item(problem: &HostIssueProblem) -> ProjectHealthItem {
    match &problem.kind {
        HostIssueProblemKind::Missing {
            github_repository,
            github_exists,
            github_title,
        } => missing_host_issue_item(
            problem,
            github_repository.as_deref(),
            *github_exists,
            github_title.as_deref(),
        ),
        HostIssueProblemKind::Mismatch {
            expected_title,
            expected_source,
            host_title,
            host_url,
        } => mismatched_host_issue_item(
            problem,
            expected_title.as_deref(),
            expected_source.as_deref(),
            host_title,
            host_url,
        ),
    }
}

fn missing_host_issue_item(
    problem: &HostIssueProblem,
    github_repository: Option<&str>,
    github_exists: bool,
    github_title: Option<&str>,
) -> ProjectHealthItem {
    let host_label = issue_host_label(problem.host);
    let can_copy = problem.host == IssueHost::Collapse && github_exists;
    let signal = format!("#{} is not on {host_label}", problem.number);
    let action = if can_copy {
        "Exists on GitHub; click Copy from GitHub to import it and retry pending updates".to_owned()
    } else {
        format!(
            "Stopped retrying; #{} was not found on {host_label}",
            problem.number
        )
    };
    let mut detail_parts = Vec::new();
    if let Some(repo) = github_repository
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && github_exists
    {
        detail_parts.push(format!("GitHub: {repo}#{}", problem.number));
    }
    if let Some(title) = github_title
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        detail_parts.push(title.to_owned());
    }
    if detail_parts.is_empty() && !problem.detail.trim().is_empty() {
        detail_parts.push(problem.detail.trim().to_owned());
    }
    ProjectHealthItem::new(
        host_issue_problem_key(problem.number),
        HealthSignalKind::HostIssueMissing,
        HealthSeverity::Warning,
        signal,
        action,
        problem.detected_at_ms,
    )
    .with_detail((!detail_parts.is_empty()).then(|| detail_parts.join(" · ")))
    .with_host_issue_action(Some(HostIssueAction {
        issue_number: problem.number,
        can_copy_from_github: can_copy,
        can_dismiss: true,
    }))
}

fn mismatched_host_issue_item(
    problem: &HostIssueProblem,
    expected_title: Option<&str>,
    expected_source: Option<&str>,
    host_title: &str,
    host_url: &str,
) -> ProjectHealthItem {
    let host_label = issue_host_label(problem.host);
    let signal = format!(
        "#{} on {host_label} does not match the landed change",
        problem.number
    );
    let mut detail_parts = Vec::new();
    if let Some(expected) = expected_title
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        detail_parts.push(format!("Landed: {expected}"));
    }
    if let Some(source) = expected_source
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        detail_parts.push(format!("Source: {source}"));
    }
    if !host_title.trim().is_empty() {
        detail_parts.push(format!("{host_label}: {}", host_title.trim()));
    }
    let mut item = ProjectHealthItem::new(
        host_issue_problem_key(problem.number),
        HealthSignalKind::HostIssueMismatch,
        HealthSeverity::Error,
        signal,
        "Skipped commenting and closing to avoid updating a different issue",
        problem.detected_at_ms,
    )
    .with_detail((!detail_parts.is_empty()).then(|| detail_parts.join(" · ")))
    .with_host_issue_action(Some(HostIssueAction {
        issue_number: problem.number,
        can_copy_from_github: false,
        can_dismiss: true,
    }));
    if host_url.starts_with("https://") {
        item = item.with_action_link(
            Some(host_url.to_owned()),
            format!("Open #{}", problem.number),
        );
    }
    item
}

/// Why an open issue is currently blocked from continuous dispatch (#616).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum BlockedIssueKind {
    NeedsOwner,
    Hold,
    Attempted,
    Other,
}

/// One-click owner action on a blocked issue inside a dispatch-idle alert (#616).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DispatchIdleIssueAction {
    /// Clear the stored dispatch attempt record so continuous dispatch retries the issue.
    RetryAttempt,
    /// Relabel the issue with `needs-owner`.
    MarkNeedsOwner,
    /// Lift the person hold on the issue so it returns to dispatch.
    LiftHold,
}

/// One blocked issue named in the dispatch-idle alert with its unblock actions (#616).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct BlockedDispatchIssue {
    pub number: u32,
    pub repository: String,
    pub title: String,
    pub kind: BlockedIssueKind,
    /// Human-readable reason the issue was skipped (for a hold, the hold's reason).
    pub reason: String,
    /// Validated `https://collapse.sh/...` or `https://github.com/...` URL to open the issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
    /// For `BlockedIssueKind::Attempted`, what the last attempt produced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_result: Option<String>,
    /// Optional thread identifier from the last attempt so the owner can inspect it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_thread: Option<String>,
    #[serde(default)]
    pub can_lift_hold: bool,
    #[serde(default)]
    pub can_retry_attempt: bool,
    #[serde(default)]
    pub can_mark_needs_owner: bool,
}

/// Structured payload attached to a `DispatchIdle` health item (#616).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct DispatchIdleAlert {
    pub busy: u32,
    pub slots: u32,
    /// Decimal Unix milliseconds when this idle spell began.
    pub idle_since_ms: String,
    /// Total seconds slots have been idle in this spell.
    pub idle_seconds: u32,
    #[serde(default)]
    pub blocked_issues: Vec<BlockedDispatchIssue>,
}

/// One persisted project-health history record so past idle spells remain visible (#616).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct ProjectHealthHistoryEntry {
    pub id: String,
    pub kind: HealthSignalKind,
    pub severity: HealthSeverity,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Decimal Unix milliseconds when the spell started.
    pub started_at_ms: String,
    /// Decimal Unix milliseconds when the spell ended (`None` while still active).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<String>,
    /// Elapsed idle duration in seconds.
    #[serde(default)]
    pub duration_seconds: u32,
}

/// Active idle spell timing reported by [`DispatchIdleTracker::observe`] (#616).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DispatchIdleActive {
    pub idle_since_secs: u64,
    pub idle_seconds: u64,
}

/// Outcome of one health-check tick for continuous-dispatch idle tracking (#616).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispatchIdleObservation {
    /// Whether the skip summary should be written to `velocity.log` (>= 10 min or changed).
    pub should_log: bool,
    /// `true` strictly once per idle spell when it reaches 30 minutes.
    pub raise_notification: bool,
    /// Active idle spell once it has reached 30 minutes (`None` before 30 min or when clear).
    pub active_alert: Option<DispatchIdleActive>,
    /// Persisted health-history entry to create, refresh, or finalize.
    pub history_update: Option<ProjectHealthHistoryEntry>,
}

/// Pure state machine tracking one project's continuous-dispatch idle spell (#509, #616).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispatchIdleTracker {
    pub idle_since_secs: Option<u64>,
    pub last_seen_dispatch_secs: Option<u64>,
    pub alerted: bool,
    pub last_logged_skip: Option<String>,
    pub last_history_duration_secs: Option<u64>,
    pub last_summary_detail: Option<String>,
    pub last_slot_counts: Option<(u32, u32)>,
}

/// Inputs for one tick of [`DispatchIdleTracker::observe`].
#[derive(Clone, Copy, Debug)]
pub struct DispatchIdleSample<'a> {
    pub now_secs: u64,
    pub has_free_slot: bool,
    pub has_candidates: bool,
    pub last_dispatched_secs: Option<u64>,
    pub busy: u32,
    pub slots: u32,
    pub detail: &'a str,
}

impl DispatchIdleTracker {
    /// Evaluates one poll tick and returns logging, one-shot notification, active UI alert,
    /// and history-entry actions.
    pub fn observe(&mut self, sample: DispatchIdleSample<'_>) -> DispatchIdleObservation {
        if sample.last_dispatched_secs != self.last_seen_dispatch_secs {
            let history_update = self.finalize_active_history(sample.now_secs);
            self.last_seen_dispatch_secs = sample.last_dispatched_secs;
            self.reset_spell();
            if sample.has_free_slot && sample.has_candidates {
                self.idle_since_secs = Some(sample.now_secs);
                self.last_slot_counts = Some((sample.busy, sample.slots));
            }
            return DispatchIdleObservation {
                history_update,
                ..DispatchIdleObservation::default()
            };
        }
        if !sample.has_free_slot || !sample.has_candidates {
            let history_update = self.finalize_active_history(sample.now_secs);
            self.reset_spell();
            return DispatchIdleObservation {
                history_update,
                ..DispatchIdleObservation::default()
            };
        }
        let since = *self.idle_since_secs.get_or_insert(sample.now_secs);
        let waited = sample.now_secs.saturating_sub(since);
        self.last_slot_counts = Some((sample.busy, sample.slots));
        if !sample.detail.trim().is_empty() {
            self.last_summary_detail = Some(sample.detail.trim().to_owned());
        }
        if waited < DISPATCH_IDLE_LIMIT_SECONDS {
            return DispatchIdleObservation::default();
        }
        let should_log = self.last_logged_skip.as_deref() != Some(sample.detail);
        if should_log {
            self.last_logged_skip = Some(sample.detail.to_owned());
        }
        if waited < DISPATCH_IDLE_ALERT_SECONDS {
            return DispatchIdleObservation {
                should_log,
                ..DispatchIdleObservation::default()
            };
        }
        let raise_notification = !self.alerted;
        self.alerted = true;
        let history_due = raise_notification
            || self.last_history_duration_secs.is_none_or(|previous| {
                waited.saturating_sub(previous) >= DISPATCH_IDLE_HISTORY_STEP_SECONDS
            });
        let history_update = history_due.then(|| {
            self.last_history_duration_secs = Some(waited);
            build_idle_history_entry(
                since,
                None,
                waited,
                sample.busy,
                sample.slots,
                self.last_summary_detail.as_deref(),
            )
        });
        DispatchIdleObservation {
            should_log,
            raise_notification,
            active_alert: Some(DispatchIdleActive {
                idle_since_secs: since,
                idle_seconds: waited,
            }),
            history_update,
        }
    }

    /// Immediately clears an active idle spell when something dispatches, returning the
    /// finalized history entry if the spell had already raised an alert (#616).
    pub fn record_dispatched(
        &mut self,
        dispatched_at_secs: u64,
    ) -> Option<ProjectHealthHistoryEntry> {
        let history_update = self.finalize_active_history(dispatched_at_secs);
        self.last_seen_dispatch_secs = Some(dispatched_at_secs);
        self.reset_spell();
        history_update
    }

    fn finalize_active_history(&self, ended_secs: u64) -> Option<ProjectHealthHistoryEntry> {
        if !self.alerted {
            return None;
        }
        let since = self.idle_since_secs?;
        let waited = ended_secs.saturating_sub(since);
        let (busy, slots) = self.last_slot_counts.unwrap_or((0, 0));
        Some(build_idle_history_entry(
            since,
            Some(ended_secs),
            waited,
            busy,
            slots,
            self.last_summary_detail.as_deref(),
        ))
    }

    fn reset_spell(&mut self) {
        self.idle_since_secs = None;
        self.alerted = false;
        self.last_logged_skip = None;
        self.last_history_duration_secs = None;
        self.last_summary_detail = None;
        self.last_slot_counts = None;
    }
}

fn build_idle_history_entry(
    started_secs: u64,
    ended_secs: Option<u64>,
    duration_secs: u64,
    busy: u32,
    slots: u32,
    detail: Option<&str>,
) -> ProjectHealthHistoryEntry {
    let slot_note = if slots > 0 {
        format!(" ({busy}/{slots})")
    } else {
        String::new()
    };
    let summary = if ended_secs.is_some() {
        format!(
            "Slots were idle{slot_note} for {} with nothing dispatched",
            format_duration(duration_secs)
        )
    } else {
        format!(
            "Slots idle{slot_note} for {} with nothing dispatched",
            format_duration(duration_secs)
        )
    };
    ProjectHealthHistoryEntry {
        id: format!("dispatch-idle:{started_secs}"),
        kind: HealthSignalKind::DispatchIdle,
        severity: HealthSeverity::Warning,
        summary,
        detail: detail
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned),
        started_at_ms: started_secs.saturating_mul(1000).to_string(),
        ended_at_ms: ended_secs.map(|secs| secs.saturating_mul(1000).to_string()),
        duration_seconds: u32::try_from(duration_secs).unwrap_or(u32::MAX),
    }
}

/// Formats a `DispatchIdle` project health item with structured blocked issues (#616).
#[allow(clippy::too_many_arguments)]
pub fn dispatch_idle_item(
    severity: HealthSeverity,
    busy: u32,
    slots: u32,
    active: DispatchIdleActive,
    action: String,
    detail: String,
    blocked_issues: Vec<BlockedDispatchIssue>,
    acted_at_ms: u64,
) -> ProjectHealthItem {
    let signal = format!(
        "Slots free ({busy}/{slots}) for {} with nothing dispatched",
        format_duration(active.idle_seconds)
    );
    ProjectHealthItem::new(
        DISPATCH_IDLE_KEY,
        HealthSignalKind::DispatchIdle,
        severity,
        signal,
        action,
        acted_at_ms,
    )
    .with_detail(Some(detail))
    .with_dispatch_idle(Some(DispatchIdleAlert {
        busy,
        slots,
        idle_since_ms: active.idle_since_secs.saturating_mul(1000).to_string(),
        idle_seconds: u32::try_from(active.idle_seconds).unwrap_or(u32::MAX),
        blocked_issues,
    }))
}

/// Builds the title and body for the one-shot desktop notification naming each blocked
/// issue and its reason (#616).
pub fn dispatch_idle_notification_text(
    project_name: &str,
    busy: u32,
    slots: u32,
    idle_seconds: u64,
    blocked_issues: &[BlockedDispatchIssue],
    fallback_detail: &str,
) -> (String, String) {
    let title = format!("{project_name}: agent slots idle ({busy}/{slots})");
    let header = format!(
        "Free for {} with nothing dispatched.",
        format_duration(idle_seconds)
    );
    let summary = if !blocked_issues.is_empty() {
        const MAX_SHOWN: usize = 8;
        let shown: Vec<String> = blocked_issues
            .iter()
            .take(MAX_SHOWN)
            .map(|issue| format!("#{} ({})", issue.number, issue.reason))
            .collect();
        let remaining = blocked_issues.len().saturating_sub(MAX_SHOWN);
        if remaining > 0 {
            format!("{} (+{remaining} more)", shown.join(", "))
        } else {
            shown.join(", ")
        }
    } else {
        fallback_detail.trim().to_owned()
    };
    let body = if summary.is_empty() {
        header
    } else {
        format!("{header}\n{summary}")
    };
    (title, body)
}

/// Formats an XML toast payload for Windows `ToastNotificationManager`, escaping XML
/// entities in `title` and `body` (#616).
pub fn windows_toast_xml(title: &str, body: &str) -> String {
    let escaped_title = escape_xml(title);
    let escaped_body = escape_xml(body);
    format!(
        "<toast><visual><binding template=\"ToastGeneric\"><text>{escaped_title}</text><text>{escaped_body}</text></binding></visual></toast>"
    )
}

fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// One active health signal on a project, with the automatic action taken and when (#509).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct ProjectHealthItem {
    /// Stable per-project identifier so repeated checks update one entry in place.
    pub key: String,
    pub kind: HealthSignalKind,
    pub severity: HealthSeverity,
    /// What went wrong or waited past its threshold.
    pub signal: String,
    /// The automatic action Velocity took, or why it could not act.
    pub action: String,
    /// Optional evidence such as both tips, skip reasons, last failure, or test and process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional thread to open from the strip (e.g. a fix-forward or stalled review thread).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_identifier: Option<String>,
    /// Optional validated external URL to open from the health row (e.g. Collapse settings, #611).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_url: Option<String>,
    /// Optional label for `action_url` (#611).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_label: Option<String>,
    /// Optional one-click revert metadata for `HostSourceChanged` signals (#613).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_switch_back: Option<HostSwitchBack>,
    /// Optional Copy from GitHub / Dismiss metadata for missing or mismatched host issues (#614).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_issue_action: Option<HostIssueAction>,
    /// Optional structured blocked-issue list and unblock actions for `DispatchIdle` (#616).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_idle: Option<DispatchIdleAlert>,
    /// Decimal Unix milliseconds when Velocity last acted or updated this signal.
    pub acted_at_ms: String,
}

impl ProjectHealthItem {
    pub fn new(
        key: impl Into<String>,
        kind: HealthSignalKind,
        severity: HealthSeverity,
        signal: impl Into<String>,
        action: impl Into<String>,
        acted_at_ms: u64,
    ) -> Self {
        Self {
            key: key.into(),
            kind,
            severity,
            signal: signal.into(),
            action: action.into(),
            detail: None,
            thread_identifier: None,
            action_url: None,
            action_label: None,
            host_switch_back: None,
            host_issue_action: None,
            dispatch_idle: None,
            acted_at_ms: acted_at_ms.to_string(),
        }
    }

    pub fn with_detail(mut self, detail: Option<String>) -> Self {
        self.detail = detail.filter(|text| !text.trim().is_empty());
        self
    }

    pub fn with_thread(mut self, thread: Option<u32>) -> Self {
        self.thread_identifier = thread.map(|id| id.to_string());
        self
    }

    pub fn with_action_link(mut self, url: Option<String>, label: impl Into<String>) -> Self {
        self.action_url = url.filter(|text| !text.trim().is_empty());
        self.action_label = self.action_url.as_ref().map(|_| label.into());
        self
    }

    pub fn with_host_switch_back(mut self, switch_back: Option<HostSwitchBack>) -> Self {
        self.host_switch_back = switch_back;
        self
    }

    pub fn with_host_issue_action(mut self, action: Option<HostIssueAction>) -> Self {
        self.host_issue_action = action;
        self
    }

    pub fn with_dispatch_idle(mut self, alert: Option<DispatchIdleAlert>) -> Self {
        self.dispatch_idle = alert;
        self
    }
}

/// All active health signals and recorded health history for one project (#509, #616).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct ProjectHealthView {
    pub project_identifier: u32,
    pub items: Vec<ProjectHealthItem>,
    #[serde(default)]
    pub history: Vec<ProjectHealthHistoryEntry>,
}

/// Formats a wait duration for the health strip: `14 min` or `2 h 15 min`.
pub fn format_duration(seconds: u64) -> String {
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes} min");
    }
    let hours = minutes / 60;
    let remaining = minutes % 60;
    if remaining == 0 {
        format!("{hours} h")
    } else {
        format!("{hours} h {remaining} min")
    }
}

/// A commit's first twelve characters, matching mirror and checker notices.
pub fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

/// Finds the test or suite step that was running when a post-land check was stopped (#486).
/// Scans from the end of `output` so the most recent active test wins over earlier passes.
pub fn extract_running_test(output: &str) -> Option<String> {
    let mut suite_fallback: Option<String> = None;
    for raw in output.lines().rev() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with("[Velocity]") {
            continue;
        }
        if let Some(test) = parse_rust_running_test(line) {
            return Some(test);
        }
        if let Some(test) = parse_js_running_test(line) {
            return Some(test);
        }
        if suite_fallback.is_none() {
            suite_fallback = parse_suite_heading(line);
        }
    }
    suite_fallback
}

/// Matches libtest lines for an in-flight test:
/// `test foo::bar ...` (unfinished) or `test foo::bar has been running for over 60 seconds`.
fn parse_rust_running_test(line: &str) -> Option<String> {
    let rest = line.strip_prefix("test ")?;
    if let Some(name) = rest.strip_suffix(" has been running for over 60 seconds") {
        let trimmed = name.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    let (name, trailing) = rest.split_once(" ...")?;
    let outcome = trailing.trim();
    if outcome.is_empty() {
        let trimmed = name.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_owned());
    }
    None
}

/// Matches Vitest/Playwright/node check step lines when a frontend/integration check hangs.
fn parse_js_running_test(line: &str) -> Option<String> {
    for prefix in ["RUN ", "❯ ", "Running test: "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

/// Matches `Running unittests src/lib.rs (target/debug/deps/...)` or `Running tests/...`.
fn parse_suite_heading(line: &str) -> Option<String> {
    if line.starts_with("Running ") && (line.contains("tests") || line.contains("deps")) {
        return Some(line.to_owned());
    }
    if let Some(step) = line.strip_prefix("==> ") {
        let trimmed = step.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

/// Summarises a post-land check stopped for running too long (#486).
pub fn format_stopped_check_detail(
    command: &str,
    test: Option<&str>,
    process: Option<&str>,
    reason: &str,
) -> String {
    let mut parts = vec![format!("`{command}` ({reason})")];
    if let Some(test) = test.filter(|value| !value.trim().is_empty()) {
        parts.push(format!("test: {test}"));
    }
    if let Some(process) = process.filter(|value| !value.trim().is_empty()) {
        parts.push(format!("process: {process}"));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_unfinished_rust_test_from_log_tail() {
        let log = "\
running 3 tests
test alpha::passes ... ok
test beta::hangs_forever ...
[Velocity] Stopped after 10 minutes without new output.
";
        assert_eq!(
            extract_running_test(log).as_deref(),
            Some("beta::hangs_forever")
        );
    }

    #[test]
    fn extracts_slow_rust_test_warning() {
        let log = "\
running 2 tests
test alpha::passes ... ok
test gamma::stalled_case has been running for over 60 seconds
";
        assert_eq!(
            extract_running_test(log).as_deref(),
            Some("gamma::stalled_case")
        );
    }

    #[test]
    fn falls_back_to_suite_heading_when_no_test_started() {
        let log = "\
   Compiling velocity-store v0.1.0
     Running unittests src/lib.rs (target/debug/deps/velocity_store-1234.exe)
";
        assert_eq!(
            extract_running_test(log).as_deref(),
            Some("Running unittests src/lib.rs (target/debug/deps/velocity_store-1234.exe)")
        );
    }

    #[test]
    fn host_source_change_item_offers_one_click_switch_back_when_possible() {
        let change = HostSourceChange {
            previous_issue_host: Some(IssueHost::Collapse),
            current_issue_host: Some(IssueHost::GitHub),
            previous_ci_source: None,
            current_ci_source: None,
            reason: "Changed on startup".into(),
            detected_at_ms: 1_700_000_000_000,
        };
        let item = host_source_change_item(&change, true, true);
        assert!(item.is_some());
        if let Some(item) = item {
            assert_eq!(item.kind, HealthSignalKind::HostSourceChanged);
            assert_eq!(item.signal, "Issue host changed from Collapse to GitHub");
            assert_eq!(
                item.host_switch_back,
                Some(HostSwitchBack {
                    button_label: "Switch back to Collapse".into(),
                    keep_label: "Keep GitHub".into(),
                    can_switch_back: true,
                })
            );
        }
        // Without a Collapse mirror, switching back to Collapse issues is not directly possible.
        assert!(!host_source_change_can_switch_back(&change, false, true));

        // Collapse-mirrored projects never switch back to GitHub Actions for base CI.
        let ci_change = HostSourceChange {
            previous_issue_host: None,
            current_issue_host: None,
            previous_ci_source: Some(BaseCiSource::GitHub),
            current_ci_source: Some(BaseCiSource::Rosen),
            reason: "[mirror] remote now points to Collapse".into(),
            detected_at_ms: 1_700_000_000_000,
        };
        assert!(!host_source_change_can_switch_back(&ci_change, true, true));
        assert!(host_source_change_can_switch_back(&ci_change, false, true));
    }

    #[test]
    fn reopened_already_landed_item_shows_commit_and_import_context() {
        let item = reopened_already_landed_item(ReopenedLandedNotice {
            issue_number: 609,
            commit: "9b7f817abcdef1234567890abcdef12345678901",
            land_summary: "Fix Move to Collapse (#609)",
            issue_title: Some("Move to Collapse issue"),
            issue_url: Some("https://collapse.sh/mESmaC/velocity/issues/609"),
            reopened_by_import: true,
            detected_at_ms: 1_700_000_000_000,
        });
        assert_eq!(item.key, "reopened-landed:609");
        assert_eq!(item.kind, HealthSignalKind::ReopenedAlreadyLanded);
        assert_eq!(item.severity, HealthSeverity::Warning);
        assert_eq!(item.signal, "#609 reopened, already landed in 9b7f817abcde");
        assert!(item.action.contains("Reopened by import"));
        assert_eq!(
            item.action_url.as_deref(),
            Some("https://collapse.sh/mESmaC/velocity/issues/609")
        );
        assert_eq!(item.action_label.as_deref(), Some("Open #609"));
    }

    #[test]
    fn dispatch_idle_tracker_raises_once_per_spell_and_clears_on_dispatch() {
        // Risk (#616): idle slot alerts must fire once per 30-minute idle spell (not every tick)
        // and clear immediately with recorded idle duration when something dispatches.
        let mut tracker = DispatchIdleTracker::default();
        let detail = "#417 (already attempted at this version), #438 (hold)";

        // T=0: starts the idle spell, nothing logged or alerted yet.
        let t0 = tracker.observe(DispatchIdleSample {
            now_secs: 1_000,
            has_free_slot: true,
            has_candidates: true,
            last_dispatched_secs: None,
            busy: 0,
            slots: 3,
            detail,
        });
        assert!(!t0.should_log);
        assert!(!t0.raise_notification);
        assert!(t0.active_alert.is_none());
        assert!(t0.history_update.is_none());

        // T=10 min: logs to velocity.log, but does not raise the 30-minute alert yet.
        let t10 = tracker.observe(DispatchIdleSample {
            now_secs: 1_000 + DISPATCH_IDLE_LIMIT_SECONDS,
            has_free_slot: true,
            has_candidates: true,
            last_dispatched_secs: None,
            busy: 0,
            slots: 3,
            detail,
        });
        assert!(t10.should_log);
        assert!(!t10.raise_notification);
        assert!(t10.active_alert.is_none());

        // T=30 min: raises the notification ONCE and records an active history entry.
        let t30 = tracker.observe(DispatchIdleSample {
            now_secs: 1_000 + DISPATCH_IDLE_ALERT_SECONDS,
            has_free_slot: true,
            has_candidates: true,
            last_dispatched_secs: None,
            busy: 0,
            slots: 3,
            detail,
        });
        assert!(t30.raise_notification);
        assert_eq!(
            t30.active_alert,
            Some(DispatchIdleActive {
                idle_since_secs: 1_000,
                idle_seconds: DISPATCH_IDLE_ALERT_SECONDS,
            })
        );
        let started_history = t30.history_update.as_ref();
        assert!(started_history.is_some());
        if let Some(entry) = started_history {
            assert_eq!(entry.id, "dispatch-idle:1000");
            assert!(entry.ended_at_ms.is_none());
            assert_eq!(entry.duration_seconds, 1_800);
        }

        // T=31 min: stays active in the UI, but does NOT re-raise the notification.
        let t31 = tracker.observe(DispatchIdleSample {
            now_secs: 1_000 + DISPATCH_IDLE_ALERT_SECONDS + 60,
            has_free_slot: true,
            has_candidates: true,
            last_dispatched_secs: None,
            busy: 0,
            slots: 3,
            detail,
        });
        assert!(!t31.raise_notification);
        assert!(t31.active_alert.is_some());

        // T=70 min: something dispatches -> clears the alert and finalizes history duration.
        let dispatched_at = 1_000 + 70 * 60;
        let cleared = tracker.observe(DispatchIdleSample {
            now_secs: dispatched_at,
            has_free_slot: true,
            has_candidates: true,
            last_dispatched_secs: Some(dispatched_at),
            busy: 1,
            slots: 3,
            detail,
        });
        assert!(!cleared.raise_notification);
        assert!(cleared.active_alert.is_none());
        let finalized = cleared.history_update.as_ref();
        assert!(finalized.is_some());
        if let Some(entry) = finalized {
            assert_eq!(entry.id, "dispatch-idle:1000");
            assert_eq!(
                entry.ended_at_ms.as_deref(),
                Some(dispatched_at.saturating_mul(1000).to_string().as_str())
            );
            assert_eq!(entry.duration_seconds, 70 * 60);
        }

        // A second 30-minute idle spell raises once again under a fresh history ID.
        let second_spell = tracker.observe(DispatchIdleSample {
            now_secs: dispatched_at + DISPATCH_IDLE_ALERT_SECONDS,
            has_free_slot: true,
            has_candidates: true,
            last_dispatched_secs: Some(dispatched_at),
            busy: 1,
            slots: 3,
            detail,
        });
        assert!(second_spell.raise_notification);
        assert_eq!(
            second_spell
                .history_update
                .as_ref()
                .map(|entry| entry.id.as_str()),
            Some("dispatch-idle:5200")
        );
    }

    #[test]
    fn dispatch_idle_notification_and_toast_xml_name_blocked_issues_and_escape_xml() {
        let issues = vec![
            BlockedDispatchIssue {
                number: 438,
                repository: "mESmaC/velocity".into(),
                title: "Hardware check".into(),
                kind: BlockedIssueKind::Hold,
                reason: "hold: needs <physical> test & signoff".into(),
                issue_url: Some("https://collapse.sh/mESmaC/velocity/issues/438".into()),
                last_attempt_result: None,
                last_attempt_thread: None,
                can_lift_hold: true,
                can_retry_attempt: false,
                can_mark_needs_owner: false,
            },
            BlockedDispatchIssue {
                number: 417,
                repository: "mESmaC/velocity".into(),
                title: "Retry issue".into(),
                kind: BlockedIssueKind::Attempted,
                reason: "already attempted at this version".into(),
                issue_url: Some("https://collapse.sh/mESmaC/velocity/issues/417".into()),
                last_attempt_result: Some("No change: waited on #421".into()),
                last_attempt_thread: Some("42".into()),
                can_lift_hold: false,
                can_retry_attempt: true,
                can_mark_needs_owner: true,
            },
        ];
        let (title, body) =
            dispatch_idle_notification_text("velocity", 0, 3, 45 * 60, &issues, "fallback");
        assert_eq!(title, "velocity: agent slots idle (0/3)");
        assert!(body.contains("45 min"));
        assert!(body.contains("#438 (hold: needs <physical> test & signoff)"));
        assert!(body.contains("#417 (already attempted at this version)"));

        let xml = windows_toast_xml(&title, &body);
        assert!(xml.contains("&lt;physical&gt;"));
        assert!(xml.contains("&amp; signoff"));
        assert!(!xml.contains("<physical>"));
    }
}
