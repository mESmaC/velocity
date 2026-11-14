use anyhow::Context;
use tauri::{AppHandle, Manager, Window};
use tauri_specta::Event;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_store::projects::{ProjectList, ProjectSelectionView};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "project://selected")]
pub struct ProjectSelected {
    pub projects: ProjectList,
    pub selection: ProjectSelectionView,
}

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let store = application.state::<ManagedStateStore>();
    let store = store
        .lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let layout = application.state::<MainLayout>();
    let mut layout = layout
        .lock()
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    store.restore_project_layout(&mut layout);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn list_projects(application: AppHandle) -> Result<ProjectList, String> {
    crate::layout::off_ui_thread(move || {
        let store = application.state::<ManagedStateStore>();
        store
            .lock()
            .map(|store| store.list_projects())
            .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn switch_project(window: Window, identifier: u32) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        let store = window.state::<ManagedStateStore>();
        let layout = window.state::<MainLayout>();
        let mut store = store.lock().map_err(|error| error.to_string())?;
        let mut layout = layout.lock().map_err(|error| error.to_string())?;
        let selection = store
            .switch_project_layout(&mut layout, identifier)
            .map_err(|error| error.to_string())?;
        let snapshot = layout.snapshot();
        let projects = store.list_projects();
        drop(store);
        crate::layout::publish(&window, &snapshot, None)?;
        publish_selection(&window, projects, selection)?;
        crate::skills::recheck_suggestions(
            window.app_handle(),
            identifier,
            crate::skills::SuggestionCheck::Opened,
        );
        Ok(snapshot)
    })
    .await
}

pub fn publish_selection(
    window: &Window,
    projects: ProjectList,
    selection: ProjectSelectionView,
) -> Result<(), String> {
    ProjectSelected {
        projects,
        selection,
    }
    .emit(window)
    .context("Could not publish project selection")
    .map_err(|error| error.to_string())
}
