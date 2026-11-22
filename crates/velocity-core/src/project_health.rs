//! Per-project health checks and automatic remediation summaries (#509).
//!
//! Pure types, thresholds, and text extractors live here; Rosen reads, GitHub queries,
//! and remediation actions live in `velocity-rosen`, `velocity-threads`, and `src-tauri`.

use serde::{Deserialize, Serialize};

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
}
