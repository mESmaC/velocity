//! Compact signal digest collector for the machine-wide improvement scout (#622).
//!
//! Reads the five signal sources gracefully: optional inputs (#619 `fleet_stats`, #620
//! friction/tool-request groups, `rosen stats`, and #621 measured outcomes) are included
//! whenever present on disk or supported by the installed `rosen` binary, without failing
//! when not yet available.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::Value;
use velocity_core::improvement_scout::{
    ImprovementDigestInput, ImprovementTrigger, VelocityLogDigest, build_compact_digest,
    parse_velocity_log_since, signal_fingerprint,
};
use velocity_core::text_budget::{preview, prompt_excerpt};
use velocity_github::velocity_reports::{FeedbackKind, ReportedDiscoveries};
use velocity_store::ProjectItem;

const LOG_FILE: &str = "velocity.log";
const OLD_LOG_FILE: &str = "velocity.old.log";
const ROSEN_STATS_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ROSEN_STATS_PROJECTS: usize = 6;
const MAX_FRICTION_LINES: usize = 20;
const MAX_OUTCOME_LINES: usize = 20;

/// Collected compact digest plus its deterministic fingerprint and new log cursor.
#[derive(Clone, Debug)]
pub struct CollectedDigest {
    pub markdown: String,
    pub fingerprint: String,
    pub log_cursor_secs: u64,
}

/// Collects all five signal inputs into one compact digest (#622).
pub fn collect_digest(
    app_data: &Path,
    projects: &[ProjectItem],
    trigger: ImprovementTrigger,
    total_lands: u64,
    lands_since_last_run: u64,
    since_log_secs: u64,
) -> CollectedDigest {
    let velocity_log = read_velocity_log_digest(app_data, since_log_secs);
    let log_cursor_secs = velocity_log.latest_unix_secs.max(since_log_secs);
    let input = ImprovementDigestInput {
        trigger,
        total_lands,
        lands_since_last_run,
        fleet_stats: read_fleet_stats_summary(app_data),
        friction_groups: read_friction_below_threshold(app_data, projects),
        velocity_log,
        rosen_stats: read_rosen_stats_summary(app_data, projects),
        prior_outcomes: read_prior_outcomes_summary(app_data),
    };
    let fingerprint = signal_fingerprint(&input);
    let markdown = build_compact_digest(&input);
    CollectedDigest {
        markdown,
        fingerprint,
        log_cursor_secs,
    }
}

fn read_velocity_log_digest(app_data: &Path, since_secs: u64) -> VelocityLogDigest {
    let mut combined = String::new();
    for name in [OLD_LOG_FILE, LOG_FILE] {
        if let Ok(content) = fs::read_to_string(app_data.join(name)) {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&content);
        }
    }
    parse_velocity_log_since(&combined, since_secs)
}

/// Reads persisted `fleet_stats` / fleet ledger data from `app_data` when #619 is present.
fn read_fleet_stats_summary(app_data: &Path) -> Option<String> {
    for candidate in [
        "fleet-stats.json",
        "fleet_stats.json",
        "fleet-ledger.json",
        "fleet_ledger.json",
    ] {
        let path = app_data.join(candidate);
        if let Ok(raw) = fs::read_to_string(&path)
            && !raw.trim().is_empty()
        {
            return Some(compact_json_or_text(&raw, 3_400));
        }
    }
    // Also inspect `state.json` for a top-level `fleet_ledger` / `fleet_stats` field if #619
    // persists rollups inside the main store snapshot.
    let state_bytes = fs::read(app_data.join(velocity_store::STATE_FILE_NAME)).ok()?;
    let value: Value = serde_json::from_slice(&state_bytes).ok()?;
    for key in ["fleet_stats", "fleet_ledger", "fleet"] {
        if let Some(section) = value.get(key)
            && !is_empty_json(section)
        {
            return Some(compact_json_or_text(&section.to_string(), 3_400));
        }
    }
    None
}

/// Reads friction and tool-request groups still below the filing threshold (#620) from
/// `velocity-reports.json` (`ReportedDiscoveries::feedback_groups`).
fn read_friction_below_threshold(app_data: &Path, _projects: &[ProjectItem]) -> Option<String> {
    let saved = ReportedDiscoveries::load(app_data).ok()?;
    let mut pending: Vec<_> = saved
        .feedback_groups
        .iter()
        .filter(|group| group.issue.is_none() && !group.reports.is_empty())
        .collect();
    if pending.is_empty() {
        return None;
    }
    pending.sort_by(|left, right| {
        right
            .total_minutes()
            .cmp(&left.total_minutes())
            .then_with(|| right.distinct_sessions().cmp(&left.distinct_sessions()))
            .then_with(|| left.title.cmp(&right.title))
    });
    let lines: Vec<String> = pending
        .into_iter()
        .take(MAX_FRICTION_LINES)
        .map(|group| {
            let kind = group
                .reports
                .first()
                .map_or("friction", |report| match report.kind {
                    FeedbackKind::Friction => "friction",
                    FeedbackKind::ToolRequest => "tool_request",
                });
            let tokens = group.total_tokens();
            let token_suffix = if tokens > 0 {
                format!(", {tokens} tokens")
            } else {
                String::new()
            };
            format!(
                "- [{kind} · {}] {} ({} session(s), {} report(s), {}m{token_suffix})",
                group.repository.repository(),
                preview(&group.title, 140),
                group.distinct_sessions(),
                group.reports.len(),
                group.total_minutes(),
            )
        })
        .collect();
    Some(lines.join("\n"))
}

/// Probes `rosen stats` (mESmaC/rosen#449) across active Rosen projects when supported.
fn read_rosen_stats_summary(app_data: &Path, projects: &[ProjectItem]) -> Option<String> {
    let rosen_bin = locate_rosen_cli()?;
    let mut sections = Vec::new();
    for project in projects
        .iter()
        .filter(|project| project.rosen_in_use())
        .take(MAX_ROSEN_STATS_PROJECTS)
    {
        let server = velocity_rosen::ProjectServer::new(app_data, project.id);
        let Some(output) = run_rosen_stats(&rosen_bin, &server) else {
            continue;
        };
        sections.push(format!(
            "### {}\n{}",
            project.name,
            prompt_excerpt(output.trim(), 700, "\n[stats truncated]")
        ));
    }
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

fn locate_rosen_cli() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "rosen.exe" } else { "rosen" };
    if let Ok(folder) = velocity_rosen::binary_folder() {
        let candidate = folder.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    Some(PathBuf::from(exe_name))
}

fn run_rosen_stats(binary: &Path, server: &velocity_rosen::ProjectServer) -> Option<String> {
    let mut command = crate::process::hidden_command(binary);
    command
        .args(["stats", "--json"])
        .env("ROSEN_ENDPOINT", server.endpoint().as_str())
        .env("ROSEN_SERVER", "velocity-starts-the-rosen-server");
    let output = run_with_timeout(command, ROSEN_STATS_TIMEOUT)?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !stdout.is_empty() {
            return Some(stdout);
        }
    }
    None
}

fn run_with_timeout(mut command: Command, limit: Duration) -> Option<std::process::Output> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = command.spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if started.elapsed() >= limit => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
}

/// Reads measured outcomes of earlier `self-improvement` issues when #621 is present.
fn read_prior_outcomes_summary(app_data: &Path) -> Option<String> {
    for candidate in [
        "self-improvement-outcomes.json",
        "improvement-outcomes.json",
        "fleet-outcomes.json",
    ] {
        let path = app_data.join(candidate);
        if let Ok(raw) = fs::read_to_string(&path)
            && !raw.trim().is_empty()
        {
            return Some(compact_json_or_text(&raw, 2_200));
        }
    }
    let state_bytes = fs::read(app_data.join(velocity_store::STATE_FILE_NAME)).ok()?;
    let value: Value = serde_json::from_slice(&state_bytes).ok()?;
    for key in [
        "self_improvement_outcomes",
        "improvement_outcomes",
        "fleet_outcomes",
    ] {
        if let Some(section) = value.get(key)
            && !is_empty_json(section)
        {
            let mut lines = Vec::new();
            if let Some(array) = section.as_array() {
                for item in array.iter().rev().take(MAX_OUTCOME_LINES) {
                    lines.push(format!("- {}", preview(&item.to_string(), 180)));
                }
                if !lines.is_empty() {
                    return Some(lines.join("\n"));
                }
            }
            return Some(compact_json_or_text(&section.to_string(), 2_200));
        }
    }
    None
}

fn compact_json_or_text(raw: &str, limit: usize) -> String {
    let compact = serde_json::from_str::<Value>(raw)
        .map(|value| value.to_string())
        .unwrap_or_else(|_| raw.trim().to_owned());
    prompt_excerpt(&compact, limit, "\n[Excerpted]")
}

fn is_empty_json(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::String(text) => text.trim().is_empty(),
        _ => false,
    }
}
