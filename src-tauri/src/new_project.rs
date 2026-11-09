//! Tauri commands for the New project dialog (SPEC.md F2). The work happens in
//! `velocity-new-project`; these commands lock the managed state, call it and emit
//! the resulting events.

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_specta::Event;
use velocity_new_project::{
    CreatedProject, NewProjectDefaults, NewProjectRequest, Publication, new_project_defaults,
};

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::projects::ProjectSelected;
use crate::state::ManagedStateStore;

#[tauri::command]
#[specta::specta]
pub fn get_new_project_defaults(application: AppHandle) -> Result<NewProjectDefaults, String> {
    let home = application
        .path()
        .home_dir()
        .map_err(|error| format!("Could not find your home folder: {error}"))?;
    let settings = application
        .state::<ManagedStateStore>()
        .lock()
        .map_err(|_| String::from("Persisted state store lock is poisoned"))?
        .settings();
    Ok(new_project_defaults(&home, settings))
}

/// Browse…: the native folder picker. `None` when it is dismissed.
#[tauri::command]
#[specta::specta]
pub async fn pick_project_folder(application: AppHandle) -> Result<Option<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        application.dialog().file().blocking_pick_folder()
    })
    .await
    .map_err(|error| format!("The folder picker stopped: {error}"))?;
    picked
        .map(|folder| {
            folder
                .into_path()
                .map(|path| path.display().to_string())
                .map_err(|error| format!("Could not read the chosen folder: {error}"))
        })
        .transpose()
}

/// Create & start interview. `git` and `gh` run off the UI thread.
#[tauri::command]
#[specta::specta]
pub async fn create_project(
    application: AppHandle,
    request: NewProjectRequest,
) -> Result<CreatedProject, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let created = velocity_new_project::create_project(
            &request,
            &application.state::<ManagedStateStore>(),
            &application.state::<MainLayout>(),
            |publication| publish(&application, publication),
        )
        .map_err(|error| error.to_string())?;
        // F2: Create "starts the spec interview thread straight away".
        crate::interview::start(&application);
        Ok(created)
    })
    .await
    .map_err(|error| format!("Create stopped: {error}"))?
}

fn publish(application: &AppHandle, publication: Publication) -> Result<(), String> {
    LayoutStateChanged {
        state: publication.snapshot,
    }
    .emit(application)
    .map_err(|error| format!("Could not publish main-window state: {error}"))?;
    if let Some((projects, selection)) = publication.selection {
        ProjectSelected {
            projects,
            selection,
        }
        .emit(application)
        .map_err(|error| format!("Could not publish project selection: {error}"))?;
    }
    Ok(())
}
