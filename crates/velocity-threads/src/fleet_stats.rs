//! `fleet_stats` AIRC MCP tool backend: reads persisted fleet ledgers without mutating disk
//! and returns hourly rollups, top offenders, and optional per-issue details as JSON (#619).

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use velocity_airc::{Error, FleetStatsRequest};
use velocity_core::fleet_ledger::{FleetIssueRecord, build_window_summary, parse_window_hours};
use velocity_store::{ProjectItem, read_all_project_snapshots};

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Handles the read-only AIRC `fleet_stats` tool call from `--airc-mcp`.
pub fn handle_fleet_stats(
    app_data: &Path,
    default_project_id: &str,
    request: FleetStatsRequest<'_>,
) -> Result<Value, Error> {
    let window_hours = parse_window_hours(request.window).map_err(Error::Invalid)?;
    let projects = read_all_project_snapshots(app_data, false)
        .map_err(|error| Error::Invalid(format!("Could not read project snapshots: {error}")))?;
    let selected = select_projects(&projects, default_project_id, request.project)?;
    let now = unix_now();
    let project_reports: Vec<Value> = selected
        .iter()
        .map(|project| build_project_report(project, window_hours, now, request.issue))
        .collect();
    let issue_detail = request
        .issue
        .and_then(|number| find_issue_record(&selected, number));
    Ok(json!({
        "window_hours": window_hours,
        "generated_at": now,
        "projects": project_reports,
        "issue": issue_detail,
    }))
}

fn select_projects<'a>(
    projects: &'a [ProjectItem],
    default_project_id: &str,
    filter: Option<&str>,
) -> Result<Vec<&'a ProjectItem>, Error> {
    let Some(filter) = filter else {
        if let Ok(id) = default_project_id.parse::<u32>()
            && let Some(project) = projects.iter().find(|project| project.id == id)
        {
            return Ok(vec![project]);
        }
        return Ok(projects.iter().collect());
    };
    if filter.eq_ignore_ascii_case("all") {
        return Ok(projects.iter().collect());
    }
    let matched: Vec<&ProjectItem> = projects
        .iter()
        .filter(|project| matches_project(project, filter))
        .collect();
    if matched.is_empty() {
        return Err(Error::Invalid(format!(
            "No project matched `{filter}`; pass a project ID, name, repository or `all`"
        )));
    }
    Ok(matched)
}

fn matches_project(project: &ProjectItem, filter: &str) -> bool {
    if let Ok(id) = filter.parse::<u32>()
        && project.id == id
    {
        return true;
    }
    if project.name.eq_ignore_ascii_case(filter) {
        return true;
    }
    if project
        .collapse_repository
        .as_deref()
        .is_some_and(|repo| repo.eq_ignore_ascii_case(filter))
    {
        return true;
    }
    if project.github_remote.as_deref().is_some_and(|remote| {
        remote
            .to_ascii_lowercase()
            .contains(&filter.to_ascii_lowercase())
    }) {
        return true;
    }
    false
}

fn build_project_report(
    project: &ProjectItem,
    window_hours: u32,
    now_secs: u64,
    issue_filter: Option<u32>,
) -> Value {
    let summary = build_window_summary(&project.fleet_ledger, window_hours, now_secs);
    let repository = project
        .collapse_repository
        .clone()
        .or_else(|| project.github_remote.clone());
    let matched_issue = issue_filter.and_then(|number| {
        project
            .fleet_ledger
            .issues
            .values()
            .find(|record| record.issue == number)
    });
    json!({
        "project_id": project.id,
        "name": project.name,
        "repository": repository,
        "summary": summary,
        "matched_issue": matched_issue.map(issue_report),
    })
}

fn find_issue_record(projects: &[&ProjectItem], number: u32) -> Option<Value> {
    projects.iter().find_map(|project| {
        project
            .fleet_ledger
            .issues
            .values()
            .find(|record| record.issue == number)
            .map(|record| {
                let mut value = issue_report(record);
                if let Some(map) = value.as_object_mut() {
                    map.insert("project_id".into(), json!(project.id));
                    map.insert("project_name".into(), json!(project.name));
                }
                value
            })
    })
}

fn issue_report(record: &FleetIssueRecord) -> Value {
    json!({
        "repository": record.repository,
        "issue": record.issue,
        "title": record.title,
        "url": record.url,
        "change": record.change,
        "landed_commit": record.landed_commit,
        "thread_id": record.thread_id,
        "filed_at": record.filed_at,
        "dispatched_at": record.dispatched_at,
        "first_snapshot_at": record.first_snapshot_at,
        "named_at": record.named_at,
        "landed_at": record.landed_at,
        "post_land_green_at": record.post_land_green_at,
        "failed_or_reverted_at": record.failed_or_reverted_at,
        "closed_at": record.closed_at,
        "filed_to_landed_secs": record.filed_to_landed_secs(),
        "dispatched_to_landed_secs": record.dispatched_to_landed_secs(),
        "provider": record.provider,
        "model": record.model,
        "tokens_in": record.tokens_in,
        "tokens_out": record.tokens_out,
        "tokens_total": record.total_tokens(),
        "agent_wall_ms": record.agent_wall_ms,
        "turns": record.turns,
        "checks": record.checks,
        "restarts": record.restarts,
        "retries": record.retries,
        "post_land_failures": record.post_land_failures,
        "reverts": record.reverts,
        "review_findings": record.review_findings,
    })
}
