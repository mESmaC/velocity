//! Tool issues from agents' discoveries (F14 Discoveries). An agent marks a discovery
//! with `velocity_issue` or `rosen_issue`; this edge reads every project's channel every
//! 30 seconds and files each marked discovery on its destination repository, or adds
//! it to the open issue about the same problem. What it reported is saved in the app data
//! folder, so a restart neither loses nor repeats a report.
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_airc::{Channel, Entry};
use velocity_github::{
    IssueClient,
    velocity_reports::{self, DiscoveryReport, ReportRepository, ReportedDiscoveries},
};
use velocity_store::ProjectItem;

use crate::graph_access::IssueGraph;
use crate::state::ManagedStateStore;
use crate::threads::lock;

const PASS_INTERVAL: Duration = Duration::from_secs(30);

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    std::thread::spawn(move || {
        let mut graphs = BTreeMap::new();
        loop {
            if let Err(error) = report_all(&application, &mut graphs) {
                eprintln!("Discovery issue reports: {error}");
            }
            std::thread::sleep(PASS_INTERVAL);
        }
    });
}

fn report_all(
    application: &AppHandle,
    graphs: &mut BTreeMap<u32, IssueGraph>,
) -> Result<(), String> {
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
    graphs.retain(|identifier, _| projects.iter().any(|project| project.id == *identifier));
    let mut saved = ReportedDiscoveries::load(&app_data).map_err(|error| error.to_string())?;
    let destinations = destination_projects(&projects);
    for project in &projects {
        // One project's failure (gh signed out, offline) leaves its cursor where it was, so
        // the next pass retries; the other projects still report.
        if let Err(error) = report_project(&app_data, project, &mut saved, &destinations, graphs) {
            eprintln!(
                "Discovery issue reports for project {}: {error}",
                project.name
            );
        }
    }
    Ok(())
}

fn destination_projects(projects: &[ProjectItem]) -> BTreeMap<ReportRepository, &ProjectItem> {
    [ReportRepository::Velocity, ReportRepository::Rosen]
        .into_iter()
        .filter_map(|destination| {
            let repository = destination.repository();
            let project = projects.iter().find(|project| {
                if let Some(remote) = project.github_remote.as_deref() {
                    return remote.eq_ignore_ascii_case(repository);
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
/// pending before GitHub is asked and recorded as soon as it answers, so an attempt cut off in
/// between is matched to the issue it opened on the next pass rather than filed again.
fn report_project(
    app_data: &Path,
    project: &ProjectItem,
    saved: &mut ReportedDiscoveries,
    destinations: &BTreeMap<ReportRepository, &ProjectItem>,
    graphs: &mut BTreeMap<u32, IssueGraph>,
) -> Result<(), String> {
    let cursor = saved
        .projects
        .get(&project.id)
        .map_or(0, |item| item.cursor);
    let channel =
        Channel::open(app_data, &project.id.to_string()).map_err(|error| error.to_string())?;
    let entries = channel.read(cursor).map_err(|error| error.to_string())?;
    let Some(last) = entries.last().map(|entry| entry.sequence) else {
        return Ok(());
    };
    for entry in &entries {
        let Some((repository, title)) = destination(entry) else {
            continue;
        };
        let reports = saved.projects.entry(project.id).or_default();
        let retry = reports.pending == Some(entry.sequence);
        reports.pending = Some(entry.sequence);
        saved.save(app_data).map_err(|error| error.to_string())?;
        let report =
            DiscoveryReport::new(title, &entry.text, &entry.nick, &project.name, &entry.time);
        let graph = if let Some(project) = destinations.get(&repository) {
            graphs.entry(project.id).or_default().get(app_data, project)
        } else {
            None
        };
        let outcome = velocity_reports::report_to(
            repository,
            &report,
            &saved.recent,
            |text| graph.map_or_else(Default::default, |graph| graph.mentioned_paths(text)),
            retry,
        )
        .map_err(|error| error.to_string())?;
        let reports = saved.projects.entry(project.id).or_default();
        reports.record_issue(repository, entry.sequence, outcome.issue());
        saved.remember(repository, outcome.issue(), report);
        saved.save(app_data).map_err(|error| error.to_string())?;
    }
    let reports = saved.projects.entry(project.id).or_default();
    if reports.cursor != last {
        reports.cursor = last;
        saved.save(app_data).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn destination(entry: &Entry) -> Option<(ReportRepository, &str)> {
    if entry.kind != "discovery" {
        return None;
    }
    if let Some(title) = entry.rosen_issue.as_deref() {
        Some((ReportRepository::Rosen, title))
    } else {
        entry
            .velocity_issue
            .as_deref()
            .map(|title| (ReportRepository::Velocity, title))
    }
}
