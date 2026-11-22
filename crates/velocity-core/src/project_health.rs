//! Per-project health checks and automatic remediation summaries (#509).
//!
//! Pure types, thresholds, and text extractors live here; Rosen reads, GitHub queries,
//! and remediation actions live in `velocity-rosen`, `velocity-threads`, and `src-tauri`.

use serde::{Deserialize, Serialize};

use crate::base_ci::BaseCiSource;
use crate::collapse::IssueHost;

/// Stable project-health key for non-owner issue host or CI source changes (#613).
pub const HOST_SOURCE_CHANGED_KEY: &str = "host-source-changed";

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
/// and surfacing why each issue was skipped (#509).
pub const DISPATCH_IDLE_LIMIT_SECONDS: u64 = 10 * 60;

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
}

/// All active health signals for one project, shown on the project health strip (#509).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct ProjectHealthView {
    pub project_identifier: u32,
    pub items: Vec<ProjectHealthItem>,
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
}
