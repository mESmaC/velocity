//! Tauri commands for the New project dialog and the switcher's Open project
//! (SPEC.md F2). The work happens in
//! `velocity-new-project`; these commands lock the managed state, call it and emit
//! the resulting events.

use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_specta::Event;
use velocity_new_project::{
    CreatedProject, NewProjectDefaults, NewProjectRequest, OpenProjectRequest, Publication,
    RepositoryPicker, RepositoryPickerView, new_project_defaults,
};

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::projects::ProjectSelected;
use crate::state::ManagedStateStore;

#[tauri::command]
#[specta::specta]
pub async fn get_new_project_defaults(
    application: AppHandle,
) -> Result<NewProjectDefaults, String> {
    crate::layout::off_ui_thread(move || {
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
    })
    .await
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

/// Open project: a folder or one of your GitHub repositories. Cloning runs off the
/// UI thread. A project without SPEC.md starts its spec interview straight away.
#[tauri::command]
#[specta::specta]
pub async fn open_project(
    application: AppHandle,
    request: OpenProjectRequest,
) -> Result<CreatedProject, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let opened = velocity_new_project::open_project(
            &request,
            &application.state::<ManagedStateStore>(),
            &application.state::<MainLayout>(),
            |publication| publish(&application, publication),
        )
        .map_err(|error| error.to_string())?;
        crate::interview::start(&application);
        Ok(opened)
    })
    .await
    .map_err(|error| format!("Open project stopped: {error}"))?
}

/// The switcher's cached list of your GitHub repositories.
#[derive(Default)]
pub struct ManagedRepositoryPicker(Mutex<RepositoryPicker>);

/// Your repositories matching `query`. `gh` is asked only on the first call and when
/// `refresh` is set; every other keystroke searches the list already read.
#[tauri::command]
#[specta::specta]
pub async fn get_github_repositories(
    application: AppHandle,
    query: String,
    refresh: bool,
) -> Result<RepositoryPickerView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let picker = &application.state::<ManagedRepositoryPicker>().0;
        let lock_failed = || String::from("Repository picker lock is poisoned");
        let needs_fetch = picker
            .lock()
            .map_err(|_| lock_failed())?
            .needs_fetch(refresh);
        if needs_fetch {
            // gh runs without the lock, so a search typed meanwhile is not blocked.
            let repositories =
                velocity_github::list_your_repositories().map_err(|error| error.to_string())?;
            picker
                .lock()
                .map_err(|_| lock_failed())?
                .replace(repositories);
        }
        Ok(picker.lock().map_err(|_| lock_failed())?.view(&query))
    })
    .await
    .map_err(|error| format!("Listing your repositories stopped: {error}"))?
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
