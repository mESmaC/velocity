use std::path::PathBuf;
use tauri::{AppHandle, Manager, State};
use velocity_github::{
    PullRequest, PullRequestClient, PullRequestOperation, PullRequestReceipt, PullRequestSnapshot,
    ReviewRecord,
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
    application: AppHandle,
    store: State<'_, ManagedStateStore>,
    project_identifier: u32,
    operation: PullRequestOperation,
) -> Result<PullRequestReceipt, String> {
    let directory = project_directory(&store, project_identifier)?;
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        // Commands from the window are the user's own decisions: AIRC nick `you`.
        let record = ReviewRecord::open(&app_data, &project_identifier.to_string(), "you")?;
        PullRequestClient::discover(&directory)?
            .with_review_record(record)
            .execute(operation)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}
