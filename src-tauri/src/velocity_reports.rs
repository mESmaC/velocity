//! Tool issues from agents' discoveries (F14 Discoveries). An agent marks a discovery
//! with `velocity_issue`, `rosen_issue` or `collapse_issue`, or logs `friction` /
//! `tool_request` feedback (#620); this edge reads every project's channel every 30
//! seconds and files marked problem discoveries or groups feedback until it reaches
//! threshold. What it reported is saved in the app data folder, so a restart neither loses
//! nor repeats a report.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_airc::{DiscoveryKind, Entry};
use velocity_github::{
    IssueClient, IssueHost,
    velocity_reports::{
        self, DiscoveryReport, FeedbackKind, FeedbackOutcome, FeedbackReport, ReportRepository,
        ReportedDiscoveries, default_feedback_title, infer_feedback_repository,
        parse_minutes_from_text,
    },
};
use velocity_store::ProjectItem;

use crate::graph_access::issue_paths;
use crate::state::ManagedStateStore;
use crate::threads::lock;

const PASS_INTERVAL: Duration = Duration::from_secs(30);

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    std::thread::spawn(move || {
        loop {
            if let Err(error) = report_all(&application) {
                eprintln!("Discovery issue reports: {error}");
            }
            std::thread::sleep(PASS_INTERVAL);
        }
    });
}

fn report_all(application: &AppHandle) -> Result<(), String> {
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let projects = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store.state().projects.clone()
    };
    let mut saved = ReportedDiscoveries::load(&app_data).map_err(|error| error.to_string())?;
    let destinations = destination_projects(&projects);
    for project in &projects {
        // One project's failure (gh signed out, offline) leaves its cursor where it was, so
        // the next pass retries; the other projects still report.
        if let Err(error) = report_project(&app_data, project, &mut saved, &destinations) {
            eprintln!(
                "Discovery issue reports for project {}: {error}",
                project.name
            );
        }
    }
    Ok(())
}

fn destination_projects(projects: &[ProjectItem]) -> BTreeMap<ReportRepository, &ProjectItem> {
    [
        ReportRepository::Velocity,
        ReportRepository::Rosen,
        ReportRepository::Collapse,
    ]
    .into_iter()
    .filter_map(|destination| {
        let repository = destination.repository();
        let project = projects.iter().find(|project| {
            if project
                .collapse_repository
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(repository))
            {
                return true;
            }
            if let Some(remote) = project.github_remote.as_deref() {
                return remote.eq_ignore_ascii_case(repository);
            }
            if project.effective_issue_host() == IssueHost::Collapse {
                return false;
            }
            let leaf = repository.rsplit('/').next().unwrap_or(repository);
            let directory = Path::new(&project.repository_path);
            project.name.eq_ignore_ascii_case(leaf)
                && directory.join(".git").exists()
                && IssueClient::discover(directory)
                    .is_ok_and(|client| client.repository().eq_ignore_ascii_case(repository))
        })?;
        Some((destination, project))
    })
    .collect()
}

/// Reports the project's marked discoveries after its cursor, in order. Each one is marked
/// pending before the issue host is asked and recorded as soon as it answers, so an attempt
/// cut off in between is matched to the issue it opened on the next pass rather than filed again.
fn report_project(
    app_data: &Path,
    project: &ProjectItem,
    saved: &mut ReportedDiscoveries,
    destinations: &BTreeMap<ReportRepository, &ProjectItem>,
) -> Result<(), String> {
    let cursor = saved
        .projects
        .get(&project.id)
        .map_or(0, |item| item.cursor);
    let channel = crate::airc_access::open_project_channel(app_data, project)?;
    let entries = channel.read(cursor).map_err(|error| error.to_string())?;
    let Some(last) = entries.last().map(|entry| entry.sequence) else {
        return Ok(());
    };
    for entry in &entries {
        let Some(planned) = plan_report(project, entry) else {
            continue;
        };
        let reports = saved.projects.entry(project.id).or_default();
        let retry = reports.pending == Some(entry.sequence);
        reports.pending = Some(entry.sequence);
        saved.save(app_data).map_err(|error| error.to_string())?;
        match planned {
            PlannedReport::Problem { repository, title } => {
                process_problem(
                    app_data,
                    project,
                    entry,
                    repository,
                    &title,
                    saved,
                    destinations,
                    retry,
                )?;
            }
            PlannedReport::Feedback { repository, report } => {
                process_feedback(
                    app_data,
                    project,
                    entry,
                    repository,
                    *report,
                    saved,
                    destinations,
                    retry,
                )?;
            }
        }
    }
    let reports = saved.projects.entry(project.id).or_default();
    if reports.cursor != last {
        reports.cursor = last;
        saved.save(app_data).map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_problem(
    app_data: &Path,
    project: &ProjectItem,
    entry: &Entry,
    repository: ReportRepository,
    title: &str,
    saved: &mut ReportedDiscoveries,
    destinations: &BTreeMap<ReportRepository, &ProjectItem>,
    retry: bool,
) -> Result<(), String> {
    let report = DiscoveryReport::new(title, &entry.text, &entry.nick, &project.name, &entry.time);
    let target = destinations.get(&repository).copied();
    let (host, target_repository) = resolve_target_host(repository, target, project);
    let outcome = velocity_reports::report_to_host(
        host,
        target_repository.as_deref(),
        repository,
        &report,
        &saved.recent,
        |texts| target.map_or_else(Vec::new, |project| issue_paths(app_data, project, texts)),
        retry,
    )
    .map_err(|error| error.to_string())?;
    // Before the report is recorded: a failed link leaves it pending, and the retry
    // finds the issue it already opened and links again (#511).
    link_blockers(
        host,
        target_repository.as_deref(),
        repository,
        outcome.issue(),
        &report,
    )?;
    let reports = saved.projects.entry(project.id).or_default();
    reports.record_issue(repository, entry.sequence, outcome.issue());
    saved.remember(repository, outcome.issue(), report);
    saved.save(app_data).map_err(|error| error.to_string())
}

#[allow(clippy::too_many_arguments)]
fn process_feedback(
    app_data: &Path,
    project: &ProjectItem,
    entry: &Entry,
    repository: ReportRepository,
    report: FeedbackReport,
    saved: &mut ReportedDiscoveries,
    destinations: &BTreeMap<ReportRepository, &ProjectItem>,
    retry: bool,
) -> Result<(), String> {
    let target = destinations.get(&repository).copied();
    let (host, target_repository) = resolve_target_host(repository, target, project);
    let outcome = velocity_reports::report_feedback_to_host(
        host,
        target_repository.as_deref(),
        repository,
        &report,
        &mut saved.feedback_groups,
        |texts| target.map_or_else(Vec::new, |project| issue_paths(app_data, project, texts)),
        retry,
    )
    .map_err(|error| error.to_string())?;
    if let Some(issue) = outcome.issue() {
        let blocker_report = DiscoveryReport::new(
            &report.title,
            &report.text,
            &report.nick,
            &report.project,
            &report.time,
        );
        link_blockers(
            host,
            target_repository.as_deref(),
            repository,
            issue,
            &blocker_report,
        )?;
        if let FeedbackOutcome::Filed { entries, .. }
        | FeedbackOutcome::JoinedGroup { entries, .. } = &outcome
        {
            for &(proj_id, seq) in entries {
                saved
                    .projects
                    .entry(proj_id)
                    .or_default()
                    .record_issue_for_sequence(repository, seq, issue);
            }
        }
        saved.projects.entry(project.id).or_default().record_issue(
            repository,
            entry.sequence,
            issue,
        );
    } else {
        let reports = saved.projects.entry(project.id).or_default();
        reports.cursor = entry.sequence;
        reports.pending = None;
    }
    saved.save(app_data).map_err(|error| error.to_string())
}

fn resolve_target_host(
    repository: ReportRepository,
    target: Option<&ProjectItem>,
    source: &ProjectItem,
) -> (IssueHost, Option<String>) {
    match target {
        Some(dest) => (
            dest.effective_issue_host(),
            dest.issue_host_repository().map(|(_, repo)| repo),
        ),
        None => (repository.default_host(source.effective_issue_host()), None),
    }
}

/// Links the blockers a discovery declares to the issue it was reported on. A blocker
/// refused by the issue host, such as an issue that does not exist, is logged and left out
/// so the project's later reports still go through.
fn link_blockers(
    host: IssueHost,
    repository_override: Option<&str>,
    repository: ReportRepository,
    issue: u32,
    report: &DiscoveryReport,
) -> Result<(), String> {
    let refused = velocity_reports::link_declared_blockers_on(
        host,
        repository_override,
        repository,
        issue,
        report,
    )
    .map_err(|error| error.to_string())?;
    if !refused.is_empty() {
        let dest = repository_override.unwrap_or_else(|| repository.repository());
        eprintln!(
            "Discovery issue reports: refused to link {} as blockers of {dest}#{issue}",
            refused.join(", "),
        );
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum PlannedReport {
    Problem {
        repository: ReportRepository,
        title: String,
    },
    Feedback {
        repository: ReportRepository,
        report: Box<FeedbackReport>,
    },
}

fn explicit_destination(entry: &Entry) -> Option<(ReportRepository, &str)> {
    if let Some(title) = entry.collapse_issue.as_deref() {
        Some((ReportRepository::Collapse, title))
    } else if let Some(title) = entry.rosen_issue.as_deref() {
        Some((ReportRepository::Rosen, title))
    } else {
        entry
            .velocity_issue
            .as_deref()
            .map(|title| (ReportRepository::Velocity, title))
    }
}

fn plan_report(project: &ProjectItem, entry: &Entry) -> Option<PlannedReport> {
    if entry.kind != "discovery" {
        return None;
    }
    let explicit = explicit_destination(entry);
    let kind = entry.discovery_kind.unwrap_or(DiscoveryKind::Problem);
    if kind == DiscoveryKind::Problem {
        let (repository, title) = explicit?;
        return Some(PlannedReport::Problem {
            repository,
            title: title.to_owned(),
        });
    }
    let feedback_kind = match kind {
        DiscoveryKind::Friction => FeedbackKind::Friction,
        DiscoveryKind::ToolRequest => FeedbackKind::ToolRequest,
        DiscoveryKind::Problem => return None,
    };
    let (repository, report) = build_feedback_report(project, entry, feedback_kind, explicit);
    Some(PlannedReport::Feedback {
        repository,
        report: Box::new(report),
    })
}

fn build_feedback_report(
    project: &ProjectItem,
    entry: &Entry,
    feedback_kind: FeedbackKind,
    explicit: Option<(ReportRepository, &str)>,
) -> (ReportRepository, FeedbackReport) {
    let tool_or_step = entry
        .friction
        .as_ref()
        .map(|item| item.tool_or_step.clone());
    let tool_name = entry
        .tool_request
        .as_ref()
        .map(|item| item.tool_name.clone());
    let would_do = entry
        .tool_request
        .as_ref()
        .map(|item| item.would_do.clone());
    let would_replace = entry
        .tool_request
        .as_ref()
        .map(|item| item.would_replace.clone());
    let saving_per_use = entry
        .tool_request
        .as_ref()
        .map(|item| item.saving_per_use.clone());
    let (repository, title) = match explicit {
        Some((repository, title)) => (repository, title.to_owned()),
        None => (
            infer_feedback_repository(&entry.text, tool_or_step.as_deref(), tool_name.as_deref()),
            default_feedback_title(
                feedback_kind,
                &entry.text,
                tool_or_step.as_deref(),
                tool_name.as_deref(),
                would_do.as_deref(),
            ),
        ),
    };
    let cost_minutes = feedback_cost_minutes(entry, feedback_kind);
    let cost_tokens = entry.friction.as_ref().and_then(|item| item.cost_tokens);
    let session = if entry.member.trim().is_empty() {
        entry.nick.clone()
    } else {
        entry.member.clone()
    };
    let report = FeedbackReport {
        kind: feedback_kind,
        title,
        text: entry.text.clone(),
        session,
        nick: entry.nick.clone(),
        project: project.name.clone(),
        project_id: project.id,
        sequence: entry.sequence,
        time: entry.time.clone(),
        cost_minutes,
        cost_tokens,
        tool_or_step,
        tool_name,
        would_do,
        would_replace,
        saving_per_use,
    };
    (repository, report)
}

fn feedback_cost_minutes(entry: &Entry, feedback_kind: FeedbackKind) -> u32 {
    match feedback_kind {
        FeedbackKind::Friction => entry
            .friction
            .as_ref()
            .and_then(|item| item.cost_minutes)
            .unwrap_or(0),
        FeedbackKind::ToolRequest => entry
            .tool_request
            .as_ref()
            .and_then(|item| {
                item.cost_minutes
                    .or_else(|| parse_minutes_from_text(&item.saving_per_use))
            })
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use velocity_airc::{FrictionReport, ToolRequestReport};

    fn test_project(id: u32, name: &str, host: IssueHost, remote: &str) -> ProjectItem {
        let mut item = ProjectItem::new(id, name.into(), format!("/tmp/{name}"));
        item.issue_host = Some(host);
        if host == IssueHost::Collapse {
            item.collapse_repository = Some(remote.into());
        } else {
            item.github_remote = Some(remote.into());
        }
        item
    }

    fn discovery_entry(sequence: u64, text: &str) -> Entry {
        Entry {
            sequence,
            session: None,
            time: "2026-11-22T12:00:00Z".into(),
            kind: "discovery".into(),
            nick: "issue-620-369".into(),
            member: "issue-620-369".into(),
            text: text.into(),
            to: Vec::new(),
            intent: None,
            pull_request: None,
            head: None,
            change: None,
            claim: None,
            dependency: None,
            checkout: None,
            message_mode: None,
            acknowledged: None,
            files: Vec::new(),
            velocity_issue: None,
            rosen_issue: None,
            collapse_issue: None,
            discovery_kind: None,
            friction: None,
            tool_request: None,
            task_change: None,
            payload: None,
        }
    }

    // Risk (#620): `collapse_issue` must route to `mESmaC/collapse.sh` using Collapse's
    // own issue host (GitHub today), never inheriting a Collapse-hosted source project's host.
    #[test]
    fn routing_handles_collapse_issue_and_feedback_hosts() {
        let velocity = test_project(1, "velocity", IssueHost::Collapse, "mESmaC/velocity");
        let collapse = test_project(2, "collapse.sh", IssueHost::GitHub, "mESmaC/collapse.sh");

        assert_eq!(
            resolve_target_host(ReportRepository::Collapse, None, &velocity),
            (IssueHost::GitHub, None)
        );
        assert_eq!(
            resolve_target_host(ReportRepository::Collapse, Some(&collapse), &velocity),
            (IssueHost::GitHub, Some("mESmaC/collapse.sh".into()))
        );

        let mut collapse_problem = discovery_entry(1, "Collapse 500 on issues endpoint");
        collapse_problem.collapse_issue = Some("Issues API returns 500".into());
        assert_eq!(
            plan_report(&velocity, &collapse_problem),
            Some(PlannedReport::Problem {
                repository: ReportRepository::Collapse,
                title: "Issues API returns 500".into(),
            })
        );

        let mut friction = discovery_entry(2, "Rosen rebase waited on lock");
        friction.discovery_kind = Some(DiscoveryKind::Friction);
        friction.friction = Some(FrictionReport {
            cost_minutes: Some(12),
            cost_tokens: None,
            tool_or_step: "rosen rebase".into(),
        });
        let Some(PlannedReport::Feedback { repository, report }) =
            plan_report(&velocity, &friction)
        else {
            panic!("expected feedback plan");
        };
        assert_eq!(repository, ReportRepository::Rosen);
        assert_eq!(report.cost_minutes, 12);

        let mut request = discovery_entry(3, "Need failing test output tool");
        request.discovery_kind = Some(DiscoveryKind::ToolRequest);
        request.velocity_issue = Some("Return only failing test output".into());
        request.tool_request = Some(ToolRequestReport {
            tool_name: "failing_test_output".into(),
            would_do: "Return only failing test output".into(),
            would_replace: "Reading full test logs".into(),
            saving_per_use: "15 minutes".into(),
            cost_minutes: None,
        });
        let Some(PlannedReport::Feedback { repository, report }) = plan_report(&velocity, &request)
        else {
            panic!("expected tool_request plan");
        };
        assert_eq!(repository, ReportRepository::Velocity);
        assert_eq!(report.cost_minutes, 15);
    }
}
