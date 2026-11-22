//! Compact signal digest collector for the machine-wide improvement scout (#622).
//!
//! Reads the five signal sources gracefully: optional inputs (#619 `fleet_stats`, #620
//! friction/tool-request groups, `rosen stats`, and #621 measured outcomes) are included
//! whenever present on disk or supported by the installed `rosen` binary, without failing
//! when not yet available.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use velocity_core::fleet_ledger::{
    FleetOffender, FleetOffenders, FleetWindowSummary, build_window_summary,
};
use velocity_core::improvement_outcomes::format_outcome_digest_line;
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
const FLEET_STATS_WINDOW_HOURS: u32 = 24;
const FLEET_STATS_WEEK_HOURS: u32 = 24 * 7;
const MAX_FLEET_OFFENDERS: usize = 3;

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
    let now_secs = unix_seconds();
    let input = ImprovementDigestInput {
        trigger,
        total_lands,
        lands_since_last_run,
        fleet_stats: read_fleet_stats_summary(projects, now_secs),
        friction_groups: read_friction_below_threshold(app_data, projects),
        velocity_log,
        rosen_stats: read_rosen_stats_summary(app_data, projects),
        prior_outcomes: read_prior_outcomes_summary(app_data, projects),
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

/// Renders compact local 24h (and 7d baseline) fleet stats rollups and top offenders
/// across all configured projects directly from each `ProjectItem::fleet_ledger` (#619, #657).
fn read_fleet_stats_summary(projects: &[ProjectItem], now_secs: u64) -> Option<String> {
    if projects.is_empty() {
        return None;
    }
    let sections: Vec<String> = projects
        .iter()
        .map(|project| format_project_fleet_summary(project, now_secs))
        .collect();
    Some(sections.join("\n\n"))
}

fn format_project_fleet_summary(project: &ProjectItem, now_secs: u64) -> String {
    let last_24h = build_window_summary(&project.fleet_ledger, FLEET_STATS_WINDOW_HOURS, now_secs);
    let last_7d = build_window_summary(&project.fleet_ledger, FLEET_STATS_WEEK_HOURS, now_secs);
    let mut lines = vec![format!("### {}", project_heading(project))];

    let active_24h = has_window_activity(&last_24h);
    let active_7d = has_window_activity(&last_7d);
    if !active_24h && !active_7d {
        lines.push("- 24h/7d: no recorded activity".to_owned());
        return lines.join("\n");
    }

    if active_24h {
        lines.push(format!("- 24h: {}", format_window_metrics(&last_24h, true)));
    } else {
        lines.push("- 24h: no activity".to_owned());
    }
    if active_7d && window_has_additional_activity(&last_7d, &last_24h) {
        lines.push(format!("- 7d: {}", format_window_metrics(&last_7d, false)));
    }
    if has_offenders(&last_24h.offenders) {
        append_offender_lines(&mut lines, "24h", &last_24h.offenders);
    } else if has_offenders(&last_7d.offenders) {
        append_offender_lines(&mut lines, "7d", &last_7d.offenders);
    }
    lines.join("\n")
}

fn project_heading(project: &ProjectItem) -> String {
    let repository = project
        .collapse_repository
        .as_deref()
        .or(project.github_remote.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match repository {
        Some(repo) if !repo.eq_ignore_ascii_case(&project.name) => {
            format!("{} ({repo})", project.name)
        }
        _ => project.name.clone(),
    }
}

fn has_window_activity(summary: &FleetWindowSummary) -> bool {
    summary.lands > 0
        || summary.issues_dispatched > 0
        || summary.idle_slot_seconds > 0
        || summary.checks_queued > 0
        || summary.post_land_failures > 0
        || summary.reverts > 0
        || summary.retries_total > 0
        || summary.review_findings > 0
        || summary.tokens_in > 0
        || summary.tokens_out > 0
}

fn window_has_additional_activity(
    last_7d: &FleetWindowSummary,
    last_24h: &FleetWindowSummary,
) -> bool {
    last_7d.lands > last_24h.lands
        || last_7d.issues_dispatched > last_24h.issues_dispatched
        || last_7d.idle_slot_seconds > last_24h.idle_slot_seconds
        || last_7d.checks_queued > last_24h.checks_queued
        || last_7d.post_land_failures > last_24h.post_land_failures
        || last_7d.reverts > last_24h.reverts
        || last_7d.retries_total > last_24h.retries_total
        || last_7d.review_findings > last_24h.review_findings
}

fn format_window_metrics(summary: &FleetWindowSummary, include_detail: bool) -> String {
    let lands_per_hour = f64::from(summary.lands) / f64::from(summary.window_hours.max(1));
    let mut parts = vec![
        format!("{} lands ({lands_per_hour:.2}/h)", summary.lands),
        format!("{} dispatched", summary.issues_dispatched),
        format!("idle {:.1}m", summary.idle_slot_minutes),
    ];
    if let Some(dispatch_secs) = summary.median_dispatched_to_landed_secs {
        let mut timing = format!("dispatch->land {}", format_duration_secs(dispatch_secs));
        if include_detail && let Some(filed_secs) = summary.median_filed_to_landed_secs {
            timing.push_str(&format!(
                " (filed->land {})",
                format_duration_secs(filed_secs)
            ));
        }
        parts.push(timing);
    } else if let Some(filed_secs) = summary.median_filed_to_landed_secs {
        parts.push(format!("filed->land {}", format_duration_secs(filed_secs)));
    }
    let tokens_total = summary.tokens_in.saturating_add(summary.tokens_out);
    if tokens_total > 0 || summary.tokens_per_landed_change.is_some() {
        let per_land = summary
            .tokens_per_landed_change
            .map(|value| format!(" ({}/land)", format_tokens_compact(value)))
            .unwrap_or_default();
        parts.push(format!(
            "tokens {}{per_land}",
            format_tokens_compact(tokens_total)
        ));
    }
    if include_detail && summary.checks_queued > 0 {
        let wait = summary
            .check_queue_wait_ms_median
            .map(|value| format!(" (wait {})", format_duration_ms(value)))
            .unwrap_or_default();
        parts.push(format!("checks {}{wait}", summary.checks_queued));
    }
    parts.push(format_count_with_rate(
        "failures",
        summary.post_land_failures,
        summary.post_land_failure_rate,
    ));
    parts.push(format_count_with_rate(
        "reverts",
        summary.reverts,
        summary.revert_rate,
    ));
    let retries = match summary.retries_per_issue {
        Some(rate) => format!("retries {} ({rate:.2}/issue)", summary.retries_total),
        None => format!("retries {}", summary.retries_total),
    };
    parts.push(retries);
    if include_detail && summary.review_findings > 0 {
        parts.push(format!("review findings {}", summary.review_findings));
    }
    parts.join(", ")
}

fn format_count_with_rate(label: &str, count: u32, rate: Option<f64>) -> String {
    match rate {
        Some(ratio) => format!("{label} {count} ({:.1}%)", ratio * 100.0),
        None => format!("{label} {count}"),
    }
}

fn has_offenders(offenders: &FleetOffenders) -> bool {
    !offenders.slowest_issues.is_empty()
        || !offenders.most_tokens.is_empty()
        || !offenders.most_retries.is_empty()
}

fn append_offender_lines(lines: &mut Vec<String>, label: &str, offenders: &FleetOffenders) {
    if !offenders.slowest_issues.is_empty() {
        let items: Vec<String> = offenders
            .slowest_issues
            .iter()
            .take(MAX_FLEET_OFFENDERS)
            .map(format_slowest_offender)
            .collect();
        lines.push(format!("- Slowest ({label}): {}", items.join(", ")));
    }
    if !offenders.most_tokens.is_empty() {
        let items: Vec<String> = offenders
            .most_tokens
            .iter()
            .take(MAX_FLEET_OFFENDERS)
            .map(format_tokens_offender)
            .collect();
        lines.push(format!("- Most tokens ({label}): {}", items.join(", ")));
    }
    if !offenders.most_retries.is_empty() {
        let items: Vec<String> = offenders
            .most_retries
            .iter()
            .take(MAX_FLEET_OFFENDERS)
            .map(format_retries_offender)
            .collect();
        lines.push(format!("- Most retries ({label}): {}", items.join(", ")));
    }
}

fn format_slowest_offender(item: &FleetOffender) -> String {
    let duration = match (item.dispatched_to_landed_secs, item.filed_to_landed_secs) {
        (Some(secs), _) => format_duration_secs(secs),
        (None, Some(secs)) => format!("{} filed->land", format_duration_secs(secs)),
        (None, None) => "?".to_owned(),
    };
    let title = format_offender_title(item);
    format!("#{} {duration}{title}", item.issue)
}

fn format_tokens_offender(item: &FleetOffender) -> String {
    let total = item.tokens_in.saturating_add(item.tokens_out);
    let title = format_offender_title(item);
    format!("#{} {}{title}", item.issue, format_tokens_compact(total))
}

fn format_retries_offender(item: &FleetOffender) -> String {
    let mut counts = Vec::new();
    if item.retries > 0 {
        let noun = if item.retries == 1 {
            "retry"
        } else {
            "retries"
        };
        counts.push(format!("{} {noun}", item.retries));
    }
    if item.restarts > 0 {
        let noun = if item.restarts == 1 {
            "restart"
        } else {
            "restarts"
        };
        counts.push(format!("{} {noun}", item.restarts));
    }
    let detail = if counts.is_empty() {
        "0 retries".to_owned()
    } else {
        counts.join("/")
    };
    let title = format_offender_title(item);
    format!("#{} {detail}{title}", item.issue)
}

fn format_offender_title(item: &FleetOffender) -> String {
    item.title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(|title| format!(" \"{}\"", preview(title, 44)))
        .unwrap_or_default()
}

fn format_tokens_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn format_duration_secs(seconds: u64) -> String {
    if seconds < 60 {
        return format!("{seconds}s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        let rem_seconds = seconds % 60;
        return if rem_seconds > 0 {
            format!("{minutes}m{rem_seconds}s")
        } else {
            format!("{minutes}m")
        };
    }
    let hours = minutes / 60;
    let rem_minutes = minutes % 60;
    if rem_minutes > 0 {
        format!("{hours}h{rem_minutes}m")
    } else {
        format!("{hours}h")
    }
}

fn format_duration_ms(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        format!("{milliseconds}ms")
    } else if milliseconds < 60_000 {
        format!("{:.1}s", milliseconds as f64 / 1_000.0)
    } else {
        format_duration_secs(milliseconds / 1_000)
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

/// Reads measured outcomes of earlier `self-improvement` issues from each project's
/// persisted `fleet_ledger.improvement_outcomes` (#621), falling back to legacy files.
fn read_prior_outcomes_summary(app_data: &Path, projects: &[ProjectItem]) -> Option<String> {
    let mut persisted: Vec<(
        &str,
        &velocity_core::improvement_outcomes::ImprovementOutcomeRecord,
    )> = projects
        .iter()
        .flat_map(|project| {
            project
                .fleet_ledger
                .improvement_outcomes
                .iter()
                .map(move |outcome| (project.name.as_str(), outcome))
        })
        .collect();
    if !persisted.is_empty() {
        persisted.sort_by(|left, right| {
            right
                .1
                .measured_at
                .cmp(&left.1.measured_at)
                .then_with(|| right.1.landed_at.cmp(&left.1.landed_at))
                .then_with(|| right.1.issue.cmp(&left.1.issue))
        });
        let lines: Vec<String> = persisted
            .into_iter()
            .take(MAX_OUTCOME_LINES)
            .map(|(project_name, outcome)| format_outcome_digest_line(project_name, outcome))
            .collect();
        return Some(lines.join("\n"));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use velocity_core::fleet_ledger::{
        FleetCheckRecord, FleetIssueRecord, HOUR_SECONDS, issue_key,
    };

    fn project_with_repo(id: u32, name: &str, repository: &str) -> ProjectItem {
        let mut project = ProjectItem::new(id, name.into(), format!("/repo/{name}"));
        project.collapse_repository = Some(repository.into());
        project
    }

    fn sample_velocity_project(recent_hour: u64) -> ProjectItem {
        let mut velocity = project_with_repo(1, "velocity", "mESmaC/velocity");
        let mut issue = FleetIssueRecord::new("mESmaC/velocity".into(), 644);
        issue.title = Some("Project pulse skips detached Rosen lands".into());
        issue.filed_at = Some(recent_hour - 2_400);
        issue.dispatched_at = Some(recent_hour + 60);
        issue.landed_at = Some(recent_hour + 1_260);
        issue.tokens_in = 120_000;
        issue.tokens_out = 30_000;
        issue.retries = 2;
        issue.restarts = 1;
        issue.review_findings = 1;
        issue.checks.push(FleetCheckRecord {
            id: "check-1".into(),
            command: "preflight".into(),
            queued_at: recent_hour + 600,
            wait_ms: 3_200,
            run_ms: 12_000,
            succeeded: true,
        });
        velocity
            .fleet_ledger
            .issues
            .insert(issue_key("mESmaC/velocity", 644), issue);
        velocity
            .fleet_ledger
            .record_idle_interval(recent_hour, recent_hour + 900, 2);
        velocity
    }

    fn sample_rosen_project(recent_hour: u64) -> ProjectItem {
        let mut rosen = project_with_repo(2, "rosen", "mESmaC/rosen");
        let mut issue = FleetIssueRecord::new("mESmaC/rosen".into(), 451);
        issue.title = Some("Add rosen stats json output".into());
        issue.dispatched_at = Some(recent_hour - 3 * 86_400);
        issue.landed_at = Some(recent_hour - 3 * 86_400 + 900);
        issue.tokens_in = 70_000;
        issue.tokens_out = 10_000;
        rosen
            .fleet_ledger
            .issues
            .insert(issue_key("mESmaC/rosen", 451), issue);
        rosen
    }

    // Risk (#657): improvement scout digests previously ignored `ProjectItem::fleet_ledger`
    // and emitted "Unavailable locally", causing wasted AIRC `fleet_stats` calls that also
    // omitted non-host projects (`rosen`, `collapse.sh`).
    #[test]
    fn fleet_stats_summary_covers_all_configured_projects_and_offenders()
    -> Result<(), Box<dyn std::error::Error>> {
        let now_secs = 1_795_300_000;
        let recent_hour = (now_secs / HOUR_SECONDS) * HOUR_SECONDS;
        let projects = [
            sample_velocity_project(recent_hour),
            sample_rosen_project(recent_hour),
            project_with_repo(3, "collapse.sh", "mESmaC/collapse.sh"),
        ];

        let summary = read_fleet_stats_summary(&projects, now_secs)
            .ok_or("configured projects must produce local fleet stats")?;
        assert!(summary.contains("### velocity (mESmaC/velocity)"));
        assert!(summary.contains("1 lands"));
        assert!(summary.contains("idle 30.0m"));
        assert!(summary.contains("tokens 150.0k (150.0k/land)"));
        assert!(summary.contains("checks 1 (wait 3.2s)"));
        assert!(summary.contains("Slowest (24h): #644 20m"));
        assert!(summary.contains("Most tokens (24h): #644 150.0k"));
        assert!(summary.contains("Most retries (24h): #644 2 retries/1 restart"));

        assert!(summary.contains("### rosen (mESmaC/rosen)"));
        assert!(summary.contains("- 24h: no activity"));
        assert!(summary.contains("- 7d: 1 lands"));
        assert!(summary.contains("Slowest (7d): #451 15m"));

        assert!(summary.contains("### collapse.sh (mESmaC/collapse.sh)"));
        assert!(summary.contains("- 24h/7d: no recorded activity"));

        // Fingerprint stays identical within the same UTC hour when ledgers do not change.
        let same_hour_later = read_fleet_stats_summary(&projects, recent_hour + 1_800);
        assert_eq!(Some(summary), same_hour_later);
        assert_eq!(read_fleet_stats_summary(&[], now_secs), None);
        Ok(())
    }

    // Risk (#657): `collect_digest` must embed local fleet stats from `ProjectItem::fleet_ledger`
    // without requiring standalone JSON files on disk or falling back to "Unavailable locally".
    #[test]
    fn collect_digest_embeds_local_fleet_stats_without_disk_files() {
        let empty_dir = std::env::temp_dir().join(format!(
            "velocity-digest-fleet-stats-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&empty_dir);
        let projects = [
            project_with_repo(1, "velocity", "mESmaC/velocity"),
            project_with_repo(2, "rosen", "mESmaC/rosen"),
            project_with_repo(3, "collapse.sh", "mESmaC/collapse.sh"),
        ];
        let collected = collect_digest(
            &empty_dir,
            &projects,
            ImprovementTrigger::LandsReached { lands: 25 },
            50,
            25,
            0,
        );
        assert!(!collected.markdown.contains("Unavailable locally"));
        assert!(
            collected
                .markdown
                .contains("### velocity (mESmaC/velocity)")
        );
        assert!(collected.markdown.contains("### rosen (mESmaC/rosen)"));
        assert!(
            collected
                .markdown
                .contains("### collapse.sh (mESmaC/collapse.sh)")
        );
    }
}
