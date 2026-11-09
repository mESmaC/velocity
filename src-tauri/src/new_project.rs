//! Tauri commands for the New project dialog (SPEC.md F2). The steps come from
//! `velocity_core::new_project::plan_new_project`; this file observes the world,
//! runs each planned step through the crates and reports the outcome.

use anyhow::{Context, anyhow, bail};
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;
use tauri_specta::Event;
use velocity_core::new_project::{
    NewProjectLocation, Observation, Refusal, SetupStep, plan_new_project,
};
use velocity_github::{RepositoryName, Visibility};
use velocity_store::{
    EffortLevel, NewProjectRecord, PendingInterview, ProjectLocationKind, ProviderKind, SpeedMode,
};

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::projects::ProjectSelected;
use crate::state::ManagedStateStore;

#[derive(Clone, Copy, Debug, serde::Deserialize, specta::Type)]
pub enum RepositoryVisibility {
    Private,
    Public,
}

#[derive(Clone, Debug, serde::Deserialize, specta::Type)]
pub enum ProjectLocationRequest {
    LocalFolder {
        path: String,
        create_if_needed: bool,
    },
    GitHubNew {
        repository: String,
        visibility: RepositoryVisibility,
        clone_to: String,
    },
    GitHubExisting {
        repository: String,
        clone_to: String,
    },
}

#[derive(Clone, Debug, serde::Deserialize, specta::Type)]
pub struct NewProjectRequest {
    pub location: ProjectLocationRequest,
    pub model_name: Option<String>,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
    pub brief: String,
}

#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct NewProjectDefaults {
    pub folder: String,
    pub clone_to: String,
    pub provider: ProviderKind,
    pub model_name: Option<String>,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
}

#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct CreatedProject {
    pub identifier: u32,
    pub name: String,
    pub repository_path: String,
}

/// What the steps so far have done: the registered project and whether the plan
/// asked to switch to it. The switch itself runs in `finish`, together with the
/// save, so the switcher and main window update in one locked step.
#[derive(Default)]
struct Progress {
    identifier: Option<u32>,
    switch_requested: bool,
}

/// The request with its path, name and repository worked out.
struct ResolvedRequest {
    location: NewProjectLocation,
    path: PathBuf,
    name: String,
    repository: Option<RepositoryName>,
    visibility: Visibility,
}

#[tauri::command]
#[specta::specta]
pub fn get_new_project_defaults(application: AppHandle) -> Result<NewProjectDefaults, String> {
    let home = application
        .path()
        .home_dir()
        .map_err(|error| format!("Could not find your home folder: {error}"))?;
    let folder = format!("{}{}", home.display(), std::path::MAIN_SEPARATOR);
    let store = application.state::<ManagedStateStore>();
    let settings = store
        .lock()
        .map_err(|_| String::from("Persisted state store lock is poisoned"))?
        .settings();
    Ok(NewProjectDefaults {
        clone_to: folder.clone(),
        folder,
        provider: settings.providers.default_provider,
        model_name: settings.providers.default_model,
        effort: settings.providers.default_effort,
        speed: settings.providers.default_speed,
    })
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

/// Create & start interview. Refusals come back before anything is created.
#[tauri::command]
#[specta::specta]
pub async fn create_project(
    application: AppHandle,
    request: NewProjectRequest,
) -> Result<CreatedProject, String> {
    tauri::async_runtime::spawn_blocking(move || create_blocking(&application, &request))
        .await
        .map_err(|error| format!("Create stopped: {error}"))?
        .map_err(|error| format!("{error:#}"))
}

fn create_blocking(
    application: &AppHandle,
    request: &NewProjectRequest,
) -> anyhow::Result<CreatedProject> {
    let resolved = resolve(&request.location)?;
    let observation = observe(application, &resolved)?;
    let steps = plan_new_project(resolved.location, observation)
        .map_err(|refusal| anyhow!(refusal_message(refusal, &resolved)))?;
    let mut progress = Progress::default();
    for step in steps {
        run_step(application, step, &resolved, request, &mut progress)?;
    }
    let identifier = progress
        .identifier
        .context("Create finished without registering the project")?;
    finish(application, identifier, progress.switch_requested)?;
    Ok(CreatedProject {
        identifier,
        name: resolved.name,
        repository_path: resolved.path.display().to_string(),
    })
}

fn resolve(location: &ProjectLocationRequest) -> anyhow::Result<ResolvedRequest> {
    match location {
        ProjectLocationRequest::LocalFolder {
            path,
            create_if_needed,
        } => {
            let path = absolute_folder(path)?;
            let name = folder_name(&path)?;
            Ok(ResolvedRequest {
                location: NewProjectLocation::LocalFolder {
                    create_if_needed: *create_if_needed,
                },
                path,
                name,
                repository: None,
                visibility: Visibility::Private,
            })
        }
        ProjectLocationRequest::GitHubNew {
            repository,
            visibility,
            clone_to,
        } => resolve_github(
            NewProjectLocation::GitHubNew,
            repository,
            clone_to,
            match visibility {
                RepositoryVisibility::Private => Visibility::Private,
                RepositoryVisibility::Public => Visibility::Public,
            },
        ),
        ProjectLocationRequest::GitHubExisting {
            repository,
            clone_to,
        } => resolve_github(
            NewProjectLocation::GitHubExisting,
            repository,
            clone_to,
            Visibility::Private,
        ),
    }
}

fn resolve_github(
    location: NewProjectLocation,
    repository: &str,
    clone_to: &str,
    visibility: Visibility,
) -> anyhow::Result<ResolvedRequest> {
    let repository = RepositoryName::parse(repository)?;
    // The clone gets its own folder named after the repository, as `git clone` does.
    let path = absolute_folder(clone_to)?.join(repository.name());
    Ok(ResolvedRequest {
        location,
        path,
        name: repository.name().to_owned(),
        repository: Some(repository),
        visibility,
    })
}

fn absolute_folder(text: &str) -> anyhow::Result<PathBuf> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        bail!("Choose a folder for the project");
    }
    let path = PathBuf::from(trimmed);
    if !path.is_absolute() {
        bail!("Use a full path, starting with a drive letter: {trimmed}");
    }
    Ok(path)
}

fn folder_name(path: &Path) -> anyhow::Result<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .with_context(|| format!("Choose a folder, not a drive: {}", path.display()))
}

/// Reads the four facts the planner decides from. GitHub is only asked about
/// GitHub locations, and the store lock is not held while `gh` runs.
fn observe(application: &AppHandle, resolved: &ResolvedRequest) -> anyhow::Result<Observation> {
    let already_a_project = application
        .state::<ManagedStateStore>()
        .lock()
        .map_err(|_| anyhow!("Persisted state store lock is poisoned"))?
        .has_project_at(&resolved.path.display().to_string());
    let (remote_exists, target_occupied) = match &resolved.repository {
        Some(repository) => (
            velocity_github::repository_exists(repository)?,
            velocity_git::folder_is_occupied(&resolved.path)?,
        ),
        None => (false, false),
    };
    Ok(Observation {
        already_a_project,
        is_repository: velocity_git::is_repository(&resolved.path),
        remote_exists,
        target_occupied,
    })
}

fn refusal_message(refusal: Refusal, resolved: &ResolvedRequest) -> String {
    let path = resolved.path.display();
    let repository = resolved
        .repository
        .as_ref()
        .map(RepositoryName::full_name)
        .unwrap_or_default();
    match refusal {
        Refusal::NotARepository => format!(
            "{path} is not a git repository. Turn on \"Create the folder and run git init if needed\"."
        ),
        Refusal::AlreadyAProject => format!("{path} is already a project. Switch to it instead."),
        Refusal::RemoteAlreadyExists => {
            format!("github.com/{repository} already exists. Choose Existing repo to clone it.")
        }
        Refusal::RemoteMissing => {
            format!("github.com/{repository} was not found, or your gh account cannot see it.")
        }
        Refusal::TargetOccupied => {
            format!("{path} already exists and is not empty. Choose another clone folder.")
        }
    }
}

fn run_step(
    application: &AppHandle,
    step: SetupStep,
    resolved: &ResolvedRequest,
    request: &NewProjectRequest,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    match step {
        SetupStep::GitInit => velocity_git::initialize_repository(&resolved.path)?,
        SetupStep::CreateRemote => {
            velocity_github::create_repository(github_repository(resolved)?, resolved.visibility)?
        }
        SetupStep::Clone => {
            velocity_github::clone_repository(github_repository(resolved)?, &resolved.path)?
        }
        SetupStep::Register => progress.identifier = Some(register(application, resolved)?),
        SetupStep::SwitchTo => progress.switch_requested = true,
        SetupStep::StartInterview => {
            let project = progress
                .identifier
                .context("The interview step ran before Register")?;
            let interview = PendingInterview {
                model_name: request.model_name.clone(),
                effort: request.effort,
                preferred_speed: request.speed,
                brief: request.brief.trim().to_owned(),
            };
            if !with_store(application, |store| {
                store.request_interview(project, interview)
            })? {
                bail!("The new project disappeared before its interview was recorded");
            }
        }
    }
    Ok(())
}

fn github_repository(resolved: &ResolvedRequest) -> anyhow::Result<&RepositoryName> {
    resolved
        .repository
        .as_ref()
        .context("A GitHub step was planned for a local folder")
}

fn register(application: &AppHandle, resolved: &ResolvedRequest) -> anyhow::Result<u32> {
    let default_branch = velocity_git::current_branch(&resolved.path)?;
    let record = NewProjectRecord {
        name: resolved.name.clone(),
        location_kind: match resolved.repository {
            Some(_) => ProjectLocationKind::GitHub,
            None => ProjectLocationKind::Local,
        },
        repository_path: resolved.path.display().to_string(),
        default_branch,
        github_remote: resolved.repository.as_ref().map(RepositoryName::full_name),
    };
    with_store(application, |store| store.register_project(record))
}

fn with_store<Output>(
    application: &AppHandle,
    update: impl FnOnce(&mut velocity_store::StateStore) -> Output,
) -> anyhow::Result<Output> {
    let store = application.state::<ManagedStateStore>();
    let mut guard = store
        .lock()
        .map_err(|_| anyhow!("Persisted state store lock is poisoned"))?;
    Ok(update(&mut guard))
}

/// Switches to the project the way the switcher does, saves, closes the dialog and
/// publishes both events. As in `projects::switch_project`, the store is locked
/// before the layout and released before the events go out, while the layout
/// lock keeps them in order with other layout updates.
fn finish(application: &AppHandle, identifier: u32, switch: bool) -> anyhow::Result<()> {
    let store = application.state::<ManagedStateStore>();
    let mut store = store
        .lock()
        .map_err(|_| anyhow!("Persisted state store lock is poisoned"))?;
    let layout = application.state::<MainLayout>();
    let mut layout = layout
        .lock()
        .map_err(|_| anyhow!("Main-window state lock is poisoned"))?;
    let selection = if switch {
        Some(store.switch_project_layout(&mut layout, identifier)?)
    } else {
        None
    };
    // The switch writes only when the active project changes; this covers the rest.
    store
        .save_if_changed()
        .context("Could not save the new project")?;
    layout.close_new_project();
    let projects = store.list_projects();
    drop(store);
    LayoutStateChanged {
        state: layout.snapshot(),
    }
    .emit(application)
    .context("Could not publish main-window state")?;
    if let Some(selection) = selection {
        ProjectSelected {
            projects,
            selection,
        }
        .emit(application)
        .context("Could not publish project selection")?;
    }
    Ok(())
}
