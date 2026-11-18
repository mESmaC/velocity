//! The code graph's per-project switch and its measurement in the usage modal (F19
//! "Measuring it"). The switch and the totals live with the project's graph in the app data
//! folder; `velocity-graph` owns them and works out the comparison.

use std::path::PathBuf;

use tauri::{AppHandle, Manager};
use velocity_graph::usage::{self, GraphComparison};

pub(crate) fn follow_rosen_base(
    application: &AppHandle,
    project: u32,
) -> Option<std::sync::mpsc::SyncSender<()>> {
    let data = app_data(application).ok()?;
    let (wake, lands) = std::sync::mpsc::sync_channel(1);
    let endpoint = velocity_rosen::ProjectServer::new(&data, project)
        .endpoint()
        .clone();
    let project = project.to_string();
    let cache = data.join("graph").join(&project);
    std::thread::spawn(move || {
        let mut index = None;
        while lands.recv().is_ok() {
            if usage::enabled(&data, &project) {
                index = velocity_graph::Index::warm_base(endpoint.clone(), &cache, index).ok();
            }
        }
    });
    Some(wake)
}

fn app_data(application: &AppHandle) -> Result<PathBuf, String> {
    application
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not find the application data folder: {error}"))
}

pub(crate) fn subsystem_map(
    application: &AppHandle,
    project: &velocity_store::ProjectItem,
) -> Option<velocity_graph::coordination::SubsystemMap> {
    let data = app_data(application).ok()?;
    usage::enabled(&data, &project.id.to_string())
        .then(|| {
            velocity_graph::coordination::subsystem_map(
                &data,
                project.id,
                std::path::Path::new(&project.repository_path),
                project.version_control == velocity_store::VersionControl::Rosen,
            )
            .ok()
        })
        .flatten()
}

/// The usage modal's code graph section for a project: its switch and the comparison.
#[tauri::command]
#[specta::specta]
pub async fn get_code_graph(
    application: AppHandle,
    project_identifier: u32,
) -> Result<GraphComparison, String> {
    crate::layout::off_ui_thread(move || {
        let app_data = app_data(&application)?;
        let usage = usage::read(&app_data, &project_identifier.to_string());
        Ok(GraphComparison::from_usage(&usage))
    })
    .await
}

/// Turns the project's graph on or off for agents started from now on. Running agents keep
/// what they started with.
#[tauri::command]
#[specta::specta]
pub async fn set_code_graph(
    application: AppHandle,
    project_identifier: u32,
    enabled: bool,
) -> Result<GraphComparison, String> {
    crate::layout::off_ui_thread(move || {
        let app_data = app_data(&application)?;
        let usage = usage::set_enabled(&app_data, &project_identifier.to_string(), enabled)
            .map_err(|error| error.to_string())?;
        Ok(GraphComparison::from_usage(&usage))
    })
    .await
}

/// Clears both sides' totals, so a comparison starts fresh.
#[tauri::command]
#[specta::specta]
pub async fn reset_code_graph_usage(
    application: AppHandle,
    project_identifier: u32,
) -> Result<GraphComparison, String> {
    crate::layout::off_ui_thread(move || {
        let app_data = app_data(&application)?;
        let usage = usage::reset(&app_data, &project_identifier.to_string())
            .map_err(|error| error.to_string())?;
        Ok(GraphComparison::from_usage(&usage))
    })
    .await
}

use velocity_graph::status::{
    BaseJob, GraphStatus, ProjectStatus, ThreadWorkspace, stage_git_base,
};
use velocity_rosen::post_land::BaseSnapshot;
use velocity_store::VersionControl;

#[tauri::command]
#[specta::specta]
pub async fn graph_status(
    application: AppHandle,
    project_identifier: u32,
) -> Result<GraphStatus, String> {
    observe_graph(application, project_identifier, false).await
}

#[tauri::command]
#[specta::specta]
pub async fn retry_graph_index(
    application: AppHandle,
    project_identifier: u32,
) -> Result<GraphStatus, String> {
    observe_graph(application, project_identifier, true).await
}

struct ProjectGraph {
    status: ProjectStatus,
    root: PathBuf,
    branch: String,
    version_control: VersionControl,
}

impl ProjectGraph {
    fn read(application: &AppHandle, identifier: u32) -> Result<Self, String> {
        let app_data = app_data(application)?;
        let store = application.state::<crate::state::ManagedStateStore>();
        let store = store.lock().map_err(|error| error.to_string())?;
        let project = store.project(identifier).ok_or("Unknown project")?;
        let workspaces = project
            .threads
            .iter()
            .filter_map(|thread| {
                let path = thread
                    .worktree_path
                    .as_ref()
                    .or(thread.worktree_path_id.as_ref())?;
                Some(ThreadWorkspace::new(
                    thread.id,
                    PathBuf::from(path),
                    thread.title.clone(),
                ))
            })
            .collect();
        let branch = if project.default_branch.trim().is_empty() {
            "master".to_owned()
        } else {
            project.default_branch.clone()
        };
        Ok(Self {
            status: ProjectStatus::new(&app_data, &identifier.to_string(), workspaces),
            root: PathBuf::from(&project.repository_path),
            branch,
            version_control: project.version_control,
        })
    }
}

enum Base {
    Git {
        root: PathBuf,
        commit: String,
    },
    Rosen {
        commit: String,
        server: velocity_rosen::ProjectServer,
    },
}

impl Base {
    async fn resolve(
        application: &AppHandle,
        identifier: u32,
        project: &ProjectGraph,
    ) -> Result<Self, String> {
        if project.version_control == VersionControl::Rosen {
            let server = crate::rosen::server(application, identifier).await?;
            let base = BaseSnapshot::read(&server)
                .await
                .map_err(|error| error.to_string())?;
            return Ok(Self::Rosen {
                commit: base.commit().to_owned(),
                server,
            });
        }
        let root = project.root.clone();
        let branch = project.branch.clone();
        crate::layout::off_ui_thread(move || {
            let commit =
                velocity_git::resolve_commit(&root, &format!("refs/remotes/origin/{branch}"))
                    .or_else(|| {
                        velocity_git::resolve_commit(&root, &format!("refs/heads/{branch}"))
                    })
                    .ok_or_else(|| format!("Could not resolve the base branch {branch}"))?;
            Ok(Self::Git { root, commit })
        })
        .await
    }

    fn commit(&self) -> &str {
        match self {
            Self::Git { commit, .. } | Self::Rosen { commit, .. } => commit,
        }
    }

    fn index(self, job: &BaseJob) -> Result<(), velocity_graph::Error> {
        match self {
            Self::Git { root, commit } => {
                let staging = tempfile::tempdir()?;
                stage_git_base(&root, &commit, staging.path())?;
                job.index(&staging.path().join("tree"))
            }
            Self::Rosen { server, .. } => job.index_rosen(server.endpoint().clone()),
        }
    }
}

async fn observe_graph(
    application: AppHandle,
    identifier: u32,
    retry: bool,
) -> Result<GraphStatus, String> {
    let project = {
        let application = application.clone();
        crate::layout::off_ui_thread(move || ProjectGraph::read(&application, identifier)).await?
    };
    let base = match Base::resolve(&application, identifier, &project).await {
        Ok(base) => base,
        Err(error) => return Ok(project.status.unavailable(error)),
    };
    let commit = base.commit().to_owned();
    let (view, job) = crate::layout::off_ui_thread(move || {
        project
            .status
            .observe(&commit, retry)
            .map_err(|error| error.to_string())
    })
    .await?;
    if let Some(job) = job {
        index_base(base, job);
    }
    Ok(view)
}

fn index_base(base: Base, job: BaseJob) {
    tauri::async_runtime::spawn_blocking(move || {
        let result = std::panic::catch_unwind(|| base.index(&job));
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => job.fail(error.to_string()),
            Err(_) => job.fail("The graph indexer stopped unexpectedly".into()),
        }
    });
}
