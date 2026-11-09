use std::path::PathBuf;
use tauri::State;
use velocity_github::{
    PullRequest, PullRequestClient, PullRequestOperation, PullRequestReceipt, PullRequestSnapshot,
};

use crate::state::ManagedStateStore;

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

#[tauri::command]
#[specta::specta]
pub async fn dispatch_pull_request(
    store: State<'_, ManagedStateStore>,
    project_identifier: u32,
    operation: PullRequestOperation,
) -> Result<PullRequestReceipt, String> {
    let directory = project_directory(&store, project_identifier)?;
    tauri::async_runtime::spawn_blocking(move || {
        PullRequestClient::discover(&directory)?.execute(operation)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}
