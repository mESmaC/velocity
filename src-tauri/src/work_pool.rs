//! Typed IPC adapter for the goal swarm's inventory; I/O runs off the UI thread.
use crate::state::ManagedStateStore;
use crate::threads::lock;
use tauri::{AppHandle, Manager};
use velocity_core::work_pool::WorkPool;

#[derive(serde::Serialize, specta::Type)]
pub struct WorkChange {
    owner: String,
    head: String,
}

#[tauri::command]
#[specta::specta]
pub async fn get_work_change(
    application: AppHandle,
    project_identifier: u32,
    change: String,
) -> Result<WorkChange, String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    let connection = velocity_rosen::connection::requests(server.endpoint())
        .await
        .map_err(|error| error.to_string())?;
    let record = connection
        .change_show(&change)
        .await
        .map_err(|error| error.to_string())?
        .change;
    Ok(WorkChange {
        owner: record.owner.to_string(),
        head: record.head.to_string(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn get_work_pool(
    application: AppHandle,
    project_identifier: u32,
) -> Result<WorkPool, String> {
    let project = {
        let managed = application.state::<ManagedStateStore>();
        let store = lock(managed.inner(), "persisted state")?;
        store
            .project(project_identifier)
            .cloned()
            .ok_or_else(|| format!("Project {project_identifier} is not registered"))?
    };
    tauri::async_runtime::spawn_blocking(move || {
        velocity_threads::work_pool::read_work_pool(&project)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}
