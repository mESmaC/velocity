//! Velocity issues from agents' discoveries (F14 Discoveries). An agent marks a discovery
//! that is about Velocity itself with `velocity_issue`; this edge reads every project's
//! channel every 30 seconds and files each marked discovery on Velocity's repository, or adds
//! it to the open issue about the same problem. What it reported is saved in the app data
//! folder, so a restart neither loses nor repeats a report.
use std::path::Path;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_airc::Channel;
use velocity_github::velocity_reports::{self, DiscoveryReport, ReportedDiscoveries};

use crate::state::ManagedStateStore;
use crate::threads::lock;

const PASS_INTERVAL: Duration = Duration::from_secs(30);

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    std::thread::spawn(move || {
        loop {
            if let Err(error) = report_all(&application) {
                eprintln!("Velocity issue reports: {error}");
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
    let projects: Vec<(u32, String)> = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .state()
            .projects
            .iter()
            .map(|project| (project.id, project.name.clone()))
            .collect()
    };
    let mut saved = ReportedDiscoveries::load(&app_data).map_err(|error| error.to_string())?;
    // Issues filed in this pass, which GitHub's issue list may not show yet. A pass is
    // long enough apart from the next that the list has caught up by then.
    let mut filed = Vec::new();
    for (project, name) in projects {
        // One project's failure (gh signed out, offline) leaves its cursor where it was, so
        // the next pass retries; the other projects still report.
        if let Err(error) = report_project(&app_data, (project, &name), &mut saved, &mut filed) {
            eprintln!("Velocity issue reports for project {name}: {error}");
        }
    }
    Ok(())
}

/// Reports the project's marked discoveries after its cursor, in order. Each one is marked
/// pending before GitHub is asked and recorded as soon as it answers, so an attempt cut off in
/// between is matched to the issue it opened on the next pass rather than filed again.
fn report_project(
    app_data: &Path,
    (project, name): (u32, &str),
    saved: &mut ReportedDiscoveries,
    filed: &mut Vec<(u32, String)>,
) -> Result<(), String> {
    let cursor = saved.projects.get(&project).map_or(0, |item| item.cursor);
    let channel =
        Channel::open(app_data, &project.to_string()).map_err(|error| error.to_string())?;
    let entries = channel.read(cursor).map_err(|error| error.to_string())?;
    let Some(last) = entries.last().map(|entry| entry.sequence) else {
        return Ok(());
    };
    for entry in &entries {
        let Some(title) = entry
            .velocity_issue
            .as_ref()
            .filter(|_| entry.kind == "discovery")
        else {
            continue;
        };
        let reports = saved.projects.entry(project).or_default();
        let retry = reports.pending == Some(entry.sequence);
        reports.pending = Some(entry.sequence);
        saved.save(app_data).map_err(|error| error.to_string())?;
        let outcome = velocity_reports::report(
            &DiscoveryReport {
                title: title.clone(),
                text: entry.text.clone(),
                nick: entry.nick.clone(),
                project: name.to_owned(),
                time: entry.time.clone(),
            },
            filed,
            retry,
        )
        .map_err(|error| error.to_string())?;
        let reports = saved.projects.entry(project).or_default();
        reports.issues.insert(entry.sequence, outcome.issue());
        reports.cursor = entry.sequence;
        reports.pending = None;
        saved.save(app_data).map_err(|error| error.to_string())?;
    }
    let reports = saved.projects.entry(project).or_default();
    if reports.cursor != last {
        reports.cursor = last;
        saved.save(app_data).map_err(|error| error.to_string())?;
    }
    Ok(())
}
