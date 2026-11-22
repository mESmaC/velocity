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

/// The project's GitHub repository for issues, base CI and releases, plus Collapse's
/// downstream GitHub repository when Collapse reports one (#574).
#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct ProjectGitHubRepository {
    pub project_identifier: u32,
    pub repository: Option<String>,
    pub collapse_repository: Option<String>,
    pub suggested_downstream: Option<String>,
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
        crate::project_health::recheck_collapse_import_if_unchecked(
            window.app_handle(),
            identifier,
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

/// Reads the project's GitHub repository for issues and CI, along with Collapse's
/// downstream GitHub repository when one is configured (#574).
#[tauri::command]
#[specta::specta]
pub async fn get_project_github_repository(
    application: AppHandle,
    project_identifier: u32,
) -> Result<ProjectGitHubRepository, String> {
    let mut view = project_github_view(&application, project_identifier).await?;
    if view.collapse_repository.is_some() && view.suggested_downstream.is_none() {
        view.suggested_downstream =
            crate::collapse_mirror::read_downstream_github(&application, project_identifier).await;
    }
    Ok(view)
}

/// Saves or clears the project's GitHub `owner/repository` for issues, base CI, releases
/// and Rosen pin bumps (#574).
#[tauri::command]
#[specta::specta]
pub async fn set_project_github_repository(
    application: AppHandle,
    project_identifier: u32,
    repository: String,
) -> Result<ProjectGitHubRepository, String> {
    let parsed = velocity_new_project::parse_github_repository(&repository)
        .map_err(|error| error.to_string())?
        .map(|name| name.full_name());
    let (changed, refresh, forwarding_context) =
        crate::state::try_with_store(&application, move |store| {
            if store.project(project_identifier).is_none() {
                return Err("Unknown project".into());
            }
            let changed = store
                .save_github_remote(project_identifier, parsed)
                .map_err(|error| error.to_string())?;
            if changed {
                let _ = store.acknowledge_host_sources(project_identifier);
            }
            let context = store.project(project_identifier).map(|project| {
                let health = project.collapse_mirror_health.as_ref();
                (
                    project.collapse_repository.clone(),
                    project.github_remote.clone(),
                    health.map(|item| item.forwarder),
                    health.and_then(|item| item.message.clone()),
                )
            });
            Ok((changed, changed.then(|| store.layout_refresh()), context))
        })
        .await?;
    if let Some(refresh) = refresh {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::layout::apply_refresh_and_publish(&handle, refresh)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    if changed {
        if let Some((collapse_repository, github_remote, forwarder, forwarder_message)) =
            forwarding_context
        {
            crate::project_health::sync_collapse_forwarding(
                &application,
                project_identifier,
                collapse_repository.as_deref(),
                github_remote.as_deref(),
                forwarder,
                forwarder_message.as_deref(),
            );
        }
        crate::project_health::sync_host_source_change(&application, project_identifier);
        crate::base_ci::wake(&application);
    }
    project_github_view(&application, project_identifier).await
}

async fn project_github_view(
    application: &AppHandle,
    project_identifier: u32,
) -> Result<ProjectGitHubRepository, String> {
    crate::state::try_with_store(application, move |store| {
        let project = store.project(project_identifier).ok_or("Unknown project")?;
        Ok(ProjectGitHubRepository {
            project_identifier: project.id,
            repository: project.github_remote.clone(),
            collapse_repository: project.collapse_repository.clone(),
            suggested_downstream: project
                .collapse_mirror_health
                .as_ref()
                .and_then(|health| health.downstream_github.clone()),
        })
    })
    .await
}
