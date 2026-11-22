//! The code graph for every agent (F19): Velocity's own executable serves each agent's
//! `graph` MCP connection, started beside AIRC in every session (see `velocity-acp`'s
//! `graph_server`).

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::duty::{DutyEventKind, DutyFeedEvent, DutyRole};
use velocity_graph::{
    PROJECT_SWITCH, SERVER_SWITCH, WARM_SWITCH, serve_project, serve_stdio, warm_stdio,
};
use velocity_rosen::ProjectServer;
use velocity_store::{ProjectItem, VersionControl};

use crate::duty::DutyEventPublished;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// `velocity --graph-mcp <app-data> <project-id> <worktree>`: serve one agent's graph tools on
/// stdin/stdout and exit. `false` when Velocity was started normally.
pub fn serve_graph_server() -> anyhow::Result<bool> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let Some((switch, rest)) = arguments.split_first() else {
        return Ok(false);
    };
    match switch.to_str() {
        Some(SERVER_SWITCH) => {
            serve_stdio(rest).context("The code graph MCP connection failed")?;
        }
        Some(PROJECT_SWITCH) => serve_project(rest).context("The project graph worker failed")?,
        // The warm pool's warming run (F19 Warming): fill the parse cache and exit.
        Some(WARM_SWITCH) => warm_stdio(rest).context("Code graph warming failed")?,
        _ => return Ok(false),
    }
    Ok(true)
}

pub(crate) fn issue_paths(
    data: &Path,
    project: &ProjectItem,
    texts: &[(&str, &str)],
) -> Vec<BTreeSet<String>> {
    let Ok(executable) = std::env::current_exe() else {
        return vec![BTreeSet::new(); texts.len()];
    };
    let endpoint = (project.version_control == VersionControl::Rosen)
        .then(|| ProjectServer::new(data, project.id).endpoint().clone());
    velocity_graph::mentioned_paths(
        &executable,
        data,
        &project.id.to_string(),
        Path::new(&project.repository_path),
        endpoint,
        texts,
    )
}

/// Resolves concrete file references named in issue text without symbol-based prediction (#649).
pub(crate) fn issue_files(
    data: &Path,
    project: &ProjectItem,
    texts: &[(&str, &str)],
) -> Vec<BTreeSet<String>> {
    let Ok(executable) = std::env::current_exe() else {
        return vec![BTreeSet::new(); texts.len()];
    };
    let endpoint = (project.version_control == VersionControl::Rosen)
        .then(|| ProjectServer::new(data, project.id).endpoint().clone());
    velocity_graph::mentioned_files(
        &executable,
        data,
        &project.id.to_string(),
        Path::new(&project.repository_path),
        endpoint,
        texts,
    )
}

/// Checks whether open or recently landed Rosen changes alter public signatures, IPC commands,
/// or IPC event types touched by the work item described in `texts` (#542).
pub(crate) fn issue_signature_warnings(
    data: &Path,
    project: &ProjectItem,
    texts: &[String],
) -> Vec<velocity_graph::StaleReferenceWarning> {
    if project.version_control != VersionControl::Rosen {
        return Vec::new();
    }
    let Ok(executable) = std::env::current_exe() else {
        return Vec::new();
    };
    let endpoint = ProjectServer::new(data, project.id).endpoint().to_string();
    velocity_graph::dispatch_signature_warnings(
        &executable,
        data,
        &project.id.to_string(),
        &endpoint,
        texts,
        None,
    )
}

/// Checks for altered public signatures, IPC commands, or event types touched by `issue_number`
/// before dispatch starts it, surfacing any warning in the duty feed and active-project toast (#542).
pub(crate) fn warn_dispatch_signatures(
    application: &AppHandle,
    project: &ProjectItem,
    issue_number: u32,
    texts: &[String],
) {
    let Ok(data) = application.path().app_data_dir() else {
        return;
    };
    let warnings = issue_signature_warnings(&data, project, texts);
    if warnings.is_empty() {
        return;
    }
    let summary = velocity_graph::signature_impact::format_warning_summary(&warnings);
    let line = format!("Issue #{issue_number} · {summary}");
    let active = publish_dispatch_warning(application, project.id, &line);
    if active {
        crate::layout::show_toast(
            application,
            format!("Warning before starting #{issue_number}: {summary}"),
        );
    }
}

fn publish_dispatch_warning(application: &AppHandle, project: u32, line: &str) -> bool {
    let store_state = application.state::<ManagedStateStore>();
    let Ok(mut store) = lock(store_state.inner(), "persisted state") else {
        return false;
    };
    let active = store.active_project().map(|item| item.id) == Some(project);
    let events = match store.update_scouts(project, |_, timestamp| {
        vec![DutyFeedEvent::new(
            timestamp,
            "Dispatch",
            DutyEventKind::Wait,
            line.to_owned(),
        )]
    }) {
        Ok(Some(events)) => events,
        Ok(None) | Err(_) => return active,
    };
    drop(store);
    if active {
        for event in events {
            report(
                DutyEventPublished::new(DutyRole::Scouts, event)
                    .emit(application)
                    .map_err(|error| error.to_string()),
            );
        }
    }
    active
}
