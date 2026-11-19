use std::path::PathBuf;
use tauri::{State, Window};
use velocity_core::layout_view::{LayoutAction, LayoutSnapshot};
use velocity_github::{PullRequest, PullRequestClient, PullRequestSnapshot};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::lock;

fn project_directory(store: &ManagedStateStore, identifier: u32) -> Result<PathBuf, String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    store
        .list_projects()
        .rows
        .into_iter()
        .find(|project| project.identifier == identifier)
        .map(|project| PathBuf::from(project.repository_path))
        .ok_or_else(|| format!("Project {identifier} is not registered"))
}

#[tauri::command]
#[specta::specta]
pub async fn list_pull_requests(
    store: State<'_, ManagedStateStore>,
    project_identifier: u32,
) -> Result<Vec<PullRequest>, String> {
    let directory = project_directory(&store, project_identifier)?;
    tauri::async_runtime::spawn_blocking(move || PullRequestClient::discover(&directory)?.list())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn get_pull_request(
    store: State<'_, ManagedStateStore>,
    project_identifier: u32,
    number: u32,
) -> Result<PullRequestSnapshot, String> {
    let directory = project_directory(&store, project_identifier)?;
    tauri::async_runtime::spawn_blocking(move || {
        PullRequestClient::discover(&directory)?.snapshot(number)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

/// Opens historical AIRC PR links on demand.
#[tauri::command]
#[specta::specta]
pub async fn open_project_pull_request(
    window: Window,
    store: State<'_, ManagedStateStore>,
    layout: State<'_, MainLayout>,
    project_identifier: u32,
    number: u32,
) -> Result<Option<LayoutSnapshot>, String> {
    let directory = {
        let mut store = lock(store.inner(), "persisted state")?;
        let project = store
            .active_project()
            .filter(|project| project.id == project_identifier)
            .ok_or("The selected project changed")?;
        if let Some(thread) = project
            .threads
            .iter()
            .find(|thread| thread.pull_request_number == Some(number))
        {
            let identifier = thread.id as usize;
            let mut layout = lock(layout.inner(), "layout")?;
            let outcome = store
                .dispatch_project_layout(&mut layout, LayoutAction::OpenThread(identifier))
                .map_err(|error| error.to_string())?;
            drop(store);
            crate::layout::publish(&window, &outcome.snapshot, outcome.intent)?;
            return Ok(Some(outcome.snapshot));
        }
        PathBuf::from(&project.repository_path)
    };
    tauri::async_runtime::spawn_blocking(move || {
        PullRequestClient::discover(&directory)?.open_in_browser(number)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    Ok(None)
}
