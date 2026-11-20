//! Tauri commands for the New project dialog and the switcher's Open project
//! (SPEC.md F2). The work happens in
//! `velocity-new-project`; these commands lock the managed state, call it and emit
//! the resulting events.

use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_specta::Event;
use velocity_new_project::{
    CreatedProject, NewProjectDefaults, NewProjectRequest, OpenProjectRequest,
    ProjectLocationRequest, ProjectMirror, Publication, RepositoryPicker, RepositoryPickerView,
    new_project_defaults, projects_directory, validate_project_root,
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
        let paths = application.path();
        let home = paths.home_dir().ok();
        let initial = home
            .as_deref()
            .map(projects_directory)
            .into_iter()
            .chain(paths.document_dir().ok())
            .find(|path| path.is_dir() && validate_project_root(path).is_ok())
            .or_else(|| home.as_deref()?.ancestors().last().map(ToOwned::to_owned));
        let mut dialog = application.dialog().file();
        if let Some(initial) = initial {
            dialog = dialog.set_directory(initial);
        }
        dialog.blocking_pick_folder()
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

/// Create a Rosen project and start its spec interview.
#[tauri::command]
#[specta::specta]
pub async fn create_project(
    application: AppHandle,
    mut request: NewProjectRequest,
) -> Result<CreatedProject, String> {
    // The initial import pushes at once, so a Collapse mirror must exist first (#427).
    crate::collapse_mirror::prepare_new_project(&mut request).await?;
    let mirrors_to_collapse = matches!(
        &request.location,
        ProjectLocationRequest::RosenFolder {
            mirror: ProjectMirror::Collapse { .. },
            ..
        }
    );
    let host = application.clone();
    let created = tauri::async_runtime::spawn_blocking(move || {
        let created = velocity_new_project::create_project(
            &request,
            &application.state::<ManagedStateStore>(),
            &application.state::<MainLayout>(),
            |publication| publish(&application, publication),
        )
        .map_err(|error| error.to_string())?;
        Ok::<_, String>(created)
    })
    .await
    .map_err(|error| format!("Create stopped: {error}"))??;
    crate::rosen::server(&host, created.identifier).await?;
    if mirrors_to_collapse {
        // The new `rosen.toml` names the Collapse root; the relay is the server's setting.
        crate::collapse_mirror::allow_relay_after_create(&host, created.identifier).await;
    }
    crate::interview::start(&host);
    Ok(created)
}

/// Open project: a folder or one of your GitHub repositories. Cloning runs off the
/// UI thread. A project without SPEC.md starts its spec interview straight away.
#[tauri::command]
#[specta::specta]
pub async fn open_project(
    application: AppHandle,
    request: OpenProjectRequest,
) -> Result<CreatedProject, String> {
    let host = application.clone();
    let opened = tauri::async_runtime::spawn_blocking(move || {
        let opened = velocity_new_project::open_project(
            &request,
            &application.state::<ManagedStateStore>(),
            &application.state::<MainLayout>(),
            |publication| publish(&application, publication),
        )
        .map_err(|error| error.to_string())?;
        Ok::<_, String>(opened)
    })
    .await
    .map_err(|error| format!("Open project stopped: {error}"))??;
    crate::rosen::server(&host, opened.identifier).await?;
    crate::interview::start(&host);
    Ok(opened)
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
