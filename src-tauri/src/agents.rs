use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::State;
use velocity_core::agents_editor::{OutlineAction, OutlineSnapshot};
use velocity_git::agents_file::AgentsFiles;

use crate::state::ManagedStateStore;

pub type ManagedAgents = Arc<Mutex<AgentsFiles>>;

fn directory(store: &ManagedStateStore, identifier: u32) -> Result<PathBuf, String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    store
        .project(identifier)
        .map(|project| PathBuf::from(&project.repository_path))
        .ok_or_else(|| "The project is no longer registered".into())
}

#[tauri::command]
#[specta::specta]
pub async fn read_agents_md(
    store: State<'_, ManagedStateStore>,
    documents: State<'_, ManagedAgents>,
    project_identifier: u32,
) -> Result<OutlineSnapshot, String> {
    let directory = directory(&store, project_identifier)?;
    let documents = Arc::clone(&documents);
    tauri::async_runtime::spawn_blocking(move || {
        documents
            .lock()
            .map_err(|error| error.to_string())?
            .read(&directory)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn edit_agents_md(
    store: State<'_, ManagedStateStore>,
    documents: State<'_, ManagedAgents>,
    project_identifier: u32,
    action: OutlineAction,
) -> Result<OutlineSnapshot, String> {
    let directory = directory(&store, project_identifier)?;
    let documents = Arc::clone(&documents);
    tauri::async_runtime::spawn_blocking(move || {
        documents
            .lock()
            .map_err(|error| error.to_string())?
            .edit(&directory, action)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn write_agents_md(
    store: State<'_, ManagedStateStore>,
    documents: State<'_, ManagedAgents>,
    project_identifier: u32,
) -> Result<OutlineSnapshot, String> {
    let directory = directory(&store, project_identifier)?;
    let documents = Arc::clone(&documents);
    tauri::async_runtime::spawn_blocking(move || {
        documents
            .lock()
            .map_err(|error| error.to_string())?
            .save(&directory)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}
