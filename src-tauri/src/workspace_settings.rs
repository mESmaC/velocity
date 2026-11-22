use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Window};
use tauri_plugin_dialog::DialogExt;
use velocity_core::thread_menu::size_label;
use velocity_core::worktree_view::{
    WorkspacePathPreview, WorkspaceSettingsEdit, WorkspaceSettingsView, WorktreeContext,
    folder_preview, settings_view,
};
use velocity_git::inventory::{measure_worktree, read_drives};
use velocity_rosen::workspace_settings::{self, LandedWorkspacePreparation, WorkspacePreparation};

use crate::layout::{MainLayout, off_ui_thread_named, run_concurrently};
use crate::state::ManagedStateStore;
use crate::threads::lock;

const REFRESH: Duration = Duration::from_secs(4);
const MEASURE_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Default)]
struct Measurements {
    paths: Mutex<BTreeMap<PathBuf, Measurement>>,
}

struct Measurement {
    started: Instant,
    size: Option<String>,
    measuring: bool,
}

pub(crate) fn initialize(application: &AppHandle) {
    application.manage(Measurements::default());
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            refresh_footer(&application).await;
            tokio::time::sleep(REFRESH).await;
        }
    });
}

async fn refresh_footer(application: &AppHandle) {
    let Ok((project, uses_rosen)) = crate::state::with_store(application, |store| {
        let project = store.active_project();
        (
            project.map(|item| item.id),
            project.is_some_and(|item| item.rosen_in_use()),
        )
    })
    .await
    else {
        return;
    };
    let mut count = None;
    if uses_rosen
        && !crate::rosen::paused(application)
        && let Some(project) = project
        && let Ok(server) = crate::rosen::observation_server(application, project)
    {
        count = workspace_settings::workspace_count(&server).await.ok();
    }
    publish_count(application, project, count);
}

fn publish_count(application: &AppHandle, project: Option<u32>, count: Option<usize>) {
    let snapshot = {
        let managed = application.state::<MainLayout>();
        let Ok(mut layout) = lock(managed.inner(), "layout state") else {
            return;
        };
        layout
            .set_workspace_count(project, count)
            .then(|| layout.snapshot())
    };
    if let Some(snapshot) = snapshot {
        crate::threads::report(crate::layout::publish(application, &snapshot, None));
    }
}

#[tauri::command]
#[specta::specta]
pub async fn get_workspace_settings(
    application: AppHandle,
) -> Result<WorkspaceSettingsView, String> {
    run_concurrently("get_workspace_settings", move || {
        let managed = application.state::<ManagedStateStore>();
        let context = lock(managed.inner(), "persisted state")?.worktree_context();
        Ok(preferences(&context, false))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_workspace_settings(
    application: AppHandle,
    edit: WorkspaceSettingsEdit,
    project_identifier: Option<u32>,
) -> Result<WorkspaceSettingsView, String> {
    off_ui_thread_named("set_workspace_settings", move || {
        let managed = application.state::<ManagedStateStore>();
        let mut store = lock(managed.inner(), "persisted state")?;
        let location_changed = store
            .edit_worktree_settings(edit, project_identifier)
            .map_err(|error| error.to_string())?;
        let context = store.worktree_context();
        drop(store);
        Ok(preferences(&context, location_changed))
    })
    .await
}

fn preferences(context: &WorktreeContext, location_changed: bool) -> WorkspaceSettingsView {
    let drives = read_drives().unwrap_or_default();
    settings_view(context, &drives, &Default::default(), &[], location_changed)
}

#[tauri::command]
#[specta::specta]
pub async fn preview_workspace_folder(
    application: AppHandle,
    folder: String,
) -> Result<WorkspacePathPreview, String> {
    run_concurrently("preview_workspace_folder", move || {
        let managed = application.state::<ManagedStateStore>();
        let store = lock(managed.inner(), "persisted state")?;
        Ok(folder_preview(&store.worktree_context(), &folder))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn browse_workspace_folder(
    application: AppHandle,
    window: Window,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        application
            .dialog()
            .file()
            .set_parent(&window)
            .blocking_pick_folder()
            .map(|path| {
                path.into_path()
                    .map_err(|error| error.to_string())?
                    .into_os_string()
                    .into_string()
                    .map_err(|_| "The folder path cannot be represented as Unicode".into())
            })
            .transpose()
    })
    .await
    .map_err(|error| error.to_string())?
}

#[derive(serde::Serialize, specta::Type)]
pub struct WorkspaceRow {
    change: String,
    path: String,
    thread: Option<crate::rosen_status::OwningThread>,
    size: Option<String>,
    state: String,
    kept: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub async fn list_workspaces(
    application: AppHandle,
    project_identifier: u32,
) -> Result<Vec<WorkspaceRow>, String> {
    let (owners, rosen_in_use) = workspace_threads(&application, project_identifier).await?;
    if !rosen_in_use {
        return Ok(Vec::new());
    }
    let server = crate::rosen::observation_server(&application, project_identifier)?;
    let rows = workspace_settings::list(&server)
        .await
        .map_err(|error| error.to_string())?;
    let paths = rows.iter().map(|row| row.path.clone());
    let sizes = workspace_sizes(&application, paths)?;
    Ok(rows
        .into_iter()
        .map(|row| WorkspaceRow {
            path: row.path.to_string_lossy().into_owned(),
            thread: owners.get(&row.change).cloned(),
            size: sizes.get(&row.path).cloned(),
            change: row.change,
            state: row.state,
            kept: row.kept,
        })
        .collect())
}

async fn workspace_threads(
    application: &AppHandle,
    project: u32,
) -> Result<(BTreeMap<String, crate::rosen_status::OwningThread>, bool), String> {
    crate::state::try_with_store(application, move |store| {
        let item = store.project(project).ok_or("Unknown project")?;
        let rosen_in_use = item.rosen_in_use();
        let owners = item
            .threads
            .iter()
            .filter_map(|thread| {
                Some((
                    thread.rosen_change.clone()?,
                    crate::rosen_status::OwningThread {
                        identifier: thread.id.to_string(),
                        title: thread.title.clone(),
                    },
                ))
            })
            .collect();
        Ok((owners, rosen_in_use))
    })
    .await
}

fn workspace_sizes(
    application: &AppHandle,
    paths: impl Iterator<Item = PathBuf>,
) -> Result<BTreeMap<PathBuf, String>, String> {
    let managed = application.state::<Measurements>();
    let mut cache = lock(&managed.paths, "workspace sizes")?;
    cache.retain(|_, entry| entry.measuring || entry.started.elapsed() < MEASURE_INTERVAL * 4);
    let mut pending = Vec::new();
    let mut sizes = BTreeMap::new();
    for path in paths {
        let entry = cache.entry(path.clone()).or_insert_with(|| Measurement {
            started: Instant::now() - MEASURE_INTERVAL,
            size: None,
            measuring: false,
        });
        if !entry.measuring && entry.started.elapsed() >= MEASURE_INTERVAL {
            entry.measuring = true;
            pending.push(path.clone());
        }
        if let Some(size) = &entry.size {
            sizes.insert(path, size.clone());
        }
    }
    drop(cache);
    if !pending.is_empty() {
        let application = application.clone();
        tauri::async_runtime::spawn_blocking(move || measure_sizes(&application, pending));
    }
    Ok(sizes)
}

fn measure_sizes(application: &AppHandle, pending: Vec<PathBuf>) {
    let managed = application.state::<Measurements>();
    let stopped = AtomicBool::new(false);
    for path in pending {
        let size = measure_worktree(&path, &stopped).ok().map(size_label);
        if let Ok(mut cache) = managed.paths.lock()
            && let Some(entry) = cache.get_mut(&path)
        {
            entry.size = size;
            entry.started = Instant::now();
            entry.measuring = false;
        }
    }
}

#[tauri::command]
#[specta::specta]
pub async fn delete_workspace(
    application: AppHandle,
    project_identifier: u32,
    change: String,
    path: PathBuf,
) -> Result<(), String> {
    let server = crate::rosen::observation_server(&application, project_identifier)?;
    workspace_settings::delete(&server, &change, &path)
        .await
        .map_err(|error| error.to_string())?;
    refresh_footer(&application).await;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn get_workspace_preparation(
    application: AppHandle,
    project_identifier: u32,
) -> Result<Option<LandedWorkspacePreparation>, String> {
    let server = crate::rosen::observation_server(&application, project_identifier)?;
    let uses_rosen = crate::state::with_store(&application, move |store| {
        store
            .project(project_identifier)
            .is_some_and(|item| item.version_control == velocity_store::VersionControl::Rosen)
    })
    .await?;
    if !uses_rosen {
        return Ok(None);
    }
    workspace_settings::preparation(&server)
        .await
        .map(Some)
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn propose_workspace_preparation(
    application: AppHandle,
    project_identifier: u32,
    expected: WorkspacePreparation,
    edited: WorkspacePreparation,
) -> Result<Option<String>, String> {
    let uses_rosen = crate::state::with_store(&application, move |store| {
        store
            .project(project_identifier)
            .is_some_and(|item| item.version_control == velocity_store::VersionControl::Rosen)
    })
    .await?;
    if !uses_rosen {
        return Err(
            "Workspace preparation is available after the project migrates to Rosen".into(),
        );
    }
    let server = crate::rosen::server(&application, project_identifier).await?;
    workspace_settings::propose_preparation(&server, &expected, &edited)
        .await
        .map_err(|error| error.to_string())
}
