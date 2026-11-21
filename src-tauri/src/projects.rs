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
    crate::layout::run_concurrently("list_projects", move || {
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
    crate::layout::run_concurrently("switch_project", move || {
        let switched = {
            let store = window.state::<ManagedStateStore>();
            let mut store = store.lock().map_err(|error| error.to_string())?;
            store
                .select_project(identifier, "")
                .map_err(|error| error.to_string())?
        };
        let snapshot = {
            let layout = window.state::<MainLayout>();
            let mut layout = layout.lock().map_err(|error| error.to_string())?;
            layout.restore_store_project(
                switched.refresh.revision,
                switched.refresh.data,
                switched.selected_thread,
                switched.search,
                switched.refresh.spec_locked,
            );
            velocity_interview::overlay_live_for_project(
                window.state::<velocity_interview::Interviews>().inner(),
                switched.refresh.active_project,
                &mut layout,
            );
            layout.snapshot()
        };
        crate::layout::publish(&window, &snapshot, None)?;
        publish_selection(&window, switched.projects, switched.selection)?;
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
