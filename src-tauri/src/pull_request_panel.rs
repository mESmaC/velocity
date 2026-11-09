//! Native scheduling and IPC adapters; GitHub projection lives in velocity-github.
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State, Window};
use tauri_specta::Event;
use velocity_core::layout_view::{LayoutAction, LayoutSnapshot};
use velocity_github::{
    PullRequestClient, PullRequestPanelRow, PullRequestPanelSnapshot, PullRequestThread,
};
use velocity_store::ProjectItem;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;

pub type PullRequestPanel = Mutex<PullRequestPanelSnapshot>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "pr://panel")]
pub struct PullRequestPanelChanged {
    pub panel: PullRequestPanelSnapshot,
}

#[derive(Clone, PartialEq, Eq)]
struct ProjectContext {
    identifier: u32,
    directory: PathBuf,
}

fn selected_project(store: &ManagedStateStore) -> Result<Option<ProjectContext>, String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    Ok(store
        .list_projects()
        .rows
        .into_iter()
        .find(|project| project.selected)
        .map(|project| ProjectContext {
            identifier: project.identifier,
            directory: project.repository_path.into(),
        }))
}

#[tauri::command]
#[specta::specta]
pub fn get_pull_request_panel(
    store: State<'_, ManagedStateStore>,
    panel: State<'_, PullRequestPanel>,
) -> Result<PullRequestPanelSnapshot, String> {
    let project = selected_project(&store)?;
    let mut panel = panel.lock().map_err(|error| error.to_string())?;
    panel.select(project.map(|project| project.identifier));
    Ok(panel.clone())
}

#[tauri::command]
#[specta::specta]
pub async fn open_project_pull_request(
    window: Window,
    store: State<'_, ManagedStateStore>,
    layout: State<'_, MainLayout>,
    panel: State<'_, PullRequestPanel>,
    project_identifier: u32,
    number: u32,
) -> Result<Option<LayoutSnapshot>, String> {
    let project = selected_project(&store)?.ok_or("No project is selected")?;
    if project.identifier != project_identifier {
        return Err("The selected project changed; refresh the pull requests".into());
    }
    let row = {
        let panel = panel.lock().map_err(|error| error.to_string())?;
        if panel.project_identifier != Some(project.identifier) {
            return Err("The selected project changed; refresh the pull requests".into());
        }
        panel
            .rows
            .iter()
            .find(|row| row.number == number)
            .cloned()
            .ok_or("The pull request is no longer listed")?
    };
    if let Some(snapshot) = open_owning_thread(&window, &store, &layout, project_identifier, &row)?
    {
        return Ok(Some(snapshot));
    }
    tauri::async_runtime::spawn_blocking(move || {
        PullRequestClient::discover(&project.directory)?.open_in_browser(number)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    Ok(None)
}

fn open_owning_thread(
    window: &Window,
    store: &ManagedStateStore,
    layout: &MainLayout,
    project_identifier: u32,
    row: &PullRequestPanelRow,
) -> Result<Option<LayoutSnapshot>, String> {
    let mut store = store.lock().map_err(|error| error.to_string())?;
    let project = store
        .state()
        .projects
        .get(store.state().active_project_index as usize)
        .filter(|project| project.id == project_identifier)
        .ok_or("The selected project changed; refresh the pull requests")?;
    let Some(identifier) = row.owning_thread(&thread_links(project)) else {
        return Ok(None);
    };
    let mut layout = layout.lock().map_err(|error| error.to_string())?;
    let outcome = store
        .dispatch_project_layout(&mut layout, LayoutAction::OpenThread(identifier as usize))
        .map_err(|error| error.to_string())?;
    drop(store);
    crate::layout::publish(window, &outcome.snapshot, outcome.intent)?;
    Ok(Some(outcome.snapshot))
}

pub fn initialize(application: &AppHandle) {
    application.manage(PullRequestPanel::default());
    let application = application.clone();
    std::thread::spawn(move || monitor(application));
}

fn monitor(application: AppHandle) {
    let mut previous = None;
    let mut refreshed = None::<Instant>;
    loop {
        let store = application.state::<ManagedStateStore>();
        if let Ok(project) = selected_project(&store) {
            let changed = previous != project;
            if changed || refreshed.is_none_or(|time| time.elapsed() >= Duration::from_secs(30)) {
                previous = project.clone();
                if let Err(error) = refresh(&application, project) {
                    eprintln!("Could not refresh pull requests: {error}");
                }
                refreshed = Some(Instant::now());
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn refresh(application: &AppHandle, project: Option<ProjectContext>) -> Result<(), String> {
    let panel = application.state::<PullRequestPanel>();
    {
        let mut panel = panel.lock().map_err(|error| error.to_string())?;
        panel.select(project.as_ref().map(|project| project.identifier));
        PullRequestPanelChanged {
            panel: panel.clone(),
        }
        .emit(application)
        .map_err(|error| error.to_string())?;
    }
    let Some(project) = project else {
        return Ok(());
    };
    let result = PullRequestClient::discover(&project.directory)
        .and_then(|client| client.panel())
        .map_err(|error| error.to_string());
    #[cfg(debug_assertions)]
    if let Ok(rows) = &result {
        eprintln!(
            "Refreshed project {} PR panel: {} open PRs",
            project.identifier,
            rows.len()
        );
    }
    let store = application.state::<ManagedStateStore>();
    if selected_project(&store)?.as_ref() != Some(&project) {
        return Ok(());
    }
    let result = result
        .map(|mut rows| {
            associate_threads(&store, project.identifier, &mut rows)?;
            Ok(rows)
        })
        .and_then(|result| result);
    let mut panel = panel.lock().map_err(|error| error.to_string())?;
    if panel.project_identifier == Some(project.identifier) && panel.update(result) {
        PullRequestPanelChanged {
            panel: panel.clone(),
        }
        .emit(application)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn associate_threads(
    store: &ManagedStateStore,
    identifier: u32,
    rows: &mut [PullRequestPanelRow],
) -> Result<(), String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    if let Some(project) = store
        .state()
        .projects
        .iter()
        .find(|project| project.id == identifier)
    {
        let threads = thread_links(project);
        for row in rows {
            row.thread_identifier = row.owning_thread(&threads);
        }
    }
    Ok(())
}

fn thread_links(project: &ProjectItem) -> Vec<PullRequestThread<'_>> {
    project
        .threads
        .iter()
        .map(|thread| PullRequestThread {
            identifier: thread.id,
            number: thread.pull_request_number,
            branch: &thread.branch_name,
        })
        .collect()
}
