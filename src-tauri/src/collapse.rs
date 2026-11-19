//! Open on Collapse (#429): the project's repository page or a land's commit page in the
//! default browser. Rust builds the URL from the saved Collapse repository, so the window
//! never names an address and the action does nothing for a project without one.

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use velocity_core::layout_view::LayoutIntent;

use crate::state::ManagedStateStore;
use crate::threads::lock;

/// `open_on_collapse` (§3.1): `commit` names a land; without it the repository page opens.
#[tauri::command]
#[specta::specta]
pub fn open_on_collapse(
    application: AppHandle,
    project_identifier: u32,
    commit: Option<String>,
) -> Result<(), String> {
    let repository = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project_identifier)
    .and_then(|project| project.collapse_repository.clone())
    .ok_or("This project has no Collapse mirror")?;
    open_page(&application, &repository, commit.as_deref())
}

/// The palette's Open on Collapse entries act on the active project.
pub(crate) fn handle_intent(application: &AppHandle, intent: &LayoutIntent) -> bool {
    let LayoutIntent::OpenOnCollapse(commit) = intent else {
        return false;
    };
    let repository = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| {
        store
            .active_project()
            .and_then(|project| project.collapse_repository.clone())
    });
    let opened = match repository {
        Ok(Some(repository)) => open_page(application, &repository, commit.as_deref()),
        Ok(None) => Err("This project has no Collapse mirror".into()),
        Err(error) => Err(error),
    };
    crate::threads::report(opened);
    true
}

fn open_page(
    application: &AppHandle,
    repository: &str,
    commit: Option<&str>,
) -> Result<(), String> {
    let page = match commit {
        Some(commit) => velocity_core::collapse::land_page(repository, commit),
        None => velocity_core::collapse::repository_page(repository),
    }
    .ok_or("That is not a Collapse repository or landed commit")?;
    application
        .opener()
        .open_url(page, None::<&str>)
        .map_err(|error| error.to_string())
}
