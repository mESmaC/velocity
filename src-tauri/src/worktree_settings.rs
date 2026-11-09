use crate::layout::{LayoutStateChanged, MainLayout};
use crate::state::ManagedStateStore;
use std::sync::Arc;
use tauri::{AppHandle, Manager, State, Window, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tauri_specta::Event;
use velocity_core::worktree_view::{WorktreeContext, WorktreeSettingsEdit, WorktreeSettingsView};
use velocity_git::inventory::{InventoryError, WorktreeRuntime, WorktreeSource};

pub type ManagedWorktrees = Arc<WorktreeRuntime>;

#[derive(Clone, serde::Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "worktree://inventory")]
pub struct WorktreeInventoryChanged {
    pub state: WorktreeSettingsView,
}

struct Source(AppHandle);

impl WorktreeSource for Source {
    fn context(&self) -> Result<WorktreeContext, InventoryError> {
        let state = self.0.state::<ManagedStateStore>();
        let store = state
            .lock()
            .map_err(|error| InventoryError::State(error.to_string()))?;
        Ok(store.worktree_context())
    }

    fn paths(&self) -> Result<Vec<String>, InventoryError> {
        let state = self.0.state::<ManagedStateStore>();
        let store = state
            .lock()
            .map_err(|error| InventoryError::State(error.to_string()))?;
        Ok(store.worktree_paths())
    }
}

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let runtime = Arc::new(WorktreeRuntime::default());
    application.manage(Arc::clone(&runtime));
    let source = Arc::new(Source(application.clone()));
    let publisher = application.clone();
    runtime.start(source, Arc::new(move |view| publish(&publisher, view)))?;
    Ok(())
}

fn publish(application: &AppHandle, view: WorktreeSettingsView) {
    if let Err(error) = publish_layout(application, &view) {
        eprintln!("{error}");
    }
    if let Err(error) = (WorktreeInventoryChanged { state: view }).emit(application) {
        eprintln!("Could not publish worktree inventory: {error}");
    }
}

fn publish_layout(application: &AppHandle, view: &WorktreeSettingsView) -> anyhow::Result<()> {
    let state = application.state::<MainLayout>();
    let mut layout = state
        .lock()
        .map_err(|error| anyhow::anyhow!("Could not update worktree footer: {error}"))?;
    if layout.set_worktree_summary(view) {
        LayoutStateChanged {
            state: layout.snapshot(),
        }
        .emit(application)?;
    }
    Ok(())
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if matches!(event, WindowEvent::Destroyed)
        && let Some(runtime) = window.try_state::<ManagedWorktrees>()
    {
        runtime.stop();
    }
}

#[tauri::command]
#[specta::specta]
pub fn list_worktrees(
    store: State<'_, ManagedStateStore>,
    runtime: State<'_, ManagedWorktrees>,
) -> Result<WorktreeSettingsView, String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    runtime
        .snapshot(&store.worktree_context())
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn set_worktree_settings(
    application: AppHandle,
    edit: WorktreeSettingsEdit,
    project_identifier: Option<u32>,
) -> Result<WorktreeSettingsView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        save_worktree_settings(&application, edit, project_identifier)
    })
    .await
    .map_err(|error| error.to_string())?
}

fn save_worktree_settings(
    application: &AppHandle,
    edit: WorktreeSettingsEdit,
    project_identifier: Option<u32>,
) -> Result<WorktreeSettingsView, String> {
    let state = application.state::<ManagedStateStore>();
    let runtime = application.state::<ManagedWorktrees>();
    let mut store = state.lock().map_err(|error| error.to_string())?;
    if store
        .edit_worktree_settings(edit, project_identifier)
        .map_err(|error| error.to_string())?
    {
        runtime
            .location_changed()
            .map_err(|error| error.to_string())?;
    }
    let view = runtime
        .snapshot(&store.worktree_context())
        .map_err(|error| error.to_string())?;
    drop(store);
    publish(application, view.clone());
    Ok(view)
}

#[tauri::command]
#[specta::specta]
pub async fn browse_worktree_folder(
    application: AppHandle,
    window: Window,
) -> Result<Option<String>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        application
            .dialog()
            .file()
            .set_parent(&window)
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| error.to_string())?;
    picked
        .map(|path| {
            path.into_path()
                .map_err(|error| error.to_string())
                .and_then(|path| {
                    path.into_os_string()
                        .into_string()
                        .map_err(|_| "The folder path cannot be represented as Unicode".into())
                })
        })
        .transpose()
}

#[tauri::command]
#[specta::specta]
pub async fn prune_worktrees(store: State<'_, ManagedStateStore>) -> Result<String, String> {
    let repository = store
        .lock()
        .map_err(|error| error.to_string())?
        .worktree_context()
        .repository_path
        .ok_or_else(|| "Select a project to prune its orphaned registrations".to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        velocity_git::prune_orphan_worktrees(std::path::Path::new(&repository))
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())?;
    Ok("Orphaned worktree registrations pruned".into())
}
