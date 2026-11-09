//! Runs Create: observe, plan, run each step, then switch, save and close.

use std::sync::{Mutex, MutexGuard};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::new_project::{NewProjectLocation, Observation, SetupStep, plan_new_project};
use velocity_github::RepositoryName;
use velocity_store::projects::{ProjectList, ProjectSelectionView};
use velocity_store::{NewProjectRecord, PendingInterview, ProjectLocationKind, StateStore};

use crate::error::NewProjectError;
use crate::request::{CreatedProject, NewProjectRequest};
use crate::resolve::{ResolvedRequest, refusal_message, resolve};

/// What the caller publishes once Create has finished: the main window, and the
/// switcher rows when the plan switched to the new project.
pub struct Publication {
    pub snapshot: LayoutSnapshot,
    pub selection: Option<(ProjectList, ProjectSelectionView)>,
}

/// What the steps so far have done. The switch itself runs in `finish`, with the
/// save, so the switcher and main window update in one locked step.
#[derive(Default)]
struct Progress {
    identifier: Option<u32>,
    switch_requested: bool,
}

/// Create & start interview. Refusals come back before anything is created. The
/// store lock is not held while `git` or `gh` runs; `publish` is called with the
/// layout still locked so its events stay in order with other layout updates.
pub fn create_project(
    request: &NewProjectRequest,
    store: &Mutex<StateStore>,
    layout: &Mutex<LayoutState>,
    publish: impl FnOnce(Publication) -> Result<(), String>,
) -> Result<CreatedProject, NewProjectError> {
    let resolved = resolve(&request.location)?;
    let already_a_project =
        lock(store, "Persisted state store")?.has_project_at(&resolved.path_text());
    let observation = observe(&resolved, already_a_project)?;
    let steps = plan_new_project(resolved.location, observation)
        .map_err(|refusal| NewProjectError::Refused(refusal_message(refusal, &resolved)))?;
    let mut progress = Progress::default();
    for step in steps {
        run_step(step, &resolved, request, store, &mut progress)?;
    }
    let identifier = progress.identifier.ok_or(NewProjectError::StepOrder(
        "Create finished without registering the project",
    ))?;
    finish(
        store,
        layout,
        identifier,
        progress.switch_requested,
        publish,
    )?;
    Ok(CreatedProject {
        identifier,
        name: resolved.name.clone(),
        repository_path: resolved.path_text(),
    })
}

/// Reads the facts the planner decides from. GitHub is asked only about GitHub
/// locations.
fn observe(
    resolved: &ResolvedRequest,
    already_a_project: bool,
) -> Result<Observation, NewProjectError> {
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

fn run_step(
    step: SetupStep,
    resolved: &ResolvedRequest,
    request: &NewProjectRequest,
    store: &Mutex<StateStore>,
    progress: &mut Progress,
) -> Result<(), NewProjectError> {
    match step {
        SetupStep::GitInit => velocity_git::initialize_repository(&resolved.path)?,
        SetupStep::CreateRemote => {
            velocity_github::create_repository(github_repository(resolved)?, resolved.visibility)?
        }
        SetupStep::Clone => clone(resolved)?,
        SetupStep::Register => progress.identifier = Some(register(resolved, store)?),
        SetupStep::SwitchTo => progress.switch_requested = true,
        SetupStep::StartInterview => {
            let project = progress.identifier.ok_or(NewProjectError::StepOrder(
                "The interview step ran before Register",
            ))?;
            let interview = PendingInterview {
                model_name: request.model_name.clone(),
                effort: request.effort,
                preferred_speed: request.speed,
                brief: request.brief.trim().to_owned(),
            };
            if !lock(store, "Persisted state store")?.request_interview(project, interview) {
                return Err(NewProjectError::StepOrder(
                    "The new project disappeared before its interview was recorded",
                ));
            }
        }
    }
    Ok(())
}

fn clone(resolved: &ResolvedRequest) -> Result<(), NewProjectError> {
    let repository = github_repository(resolved)?;
    velocity_github::clone_repository(repository, &resolved.path).map_err(|source| {
        match resolved.location {
            NewProjectLocation::GitHubNew => NewProjectError::CloneAfterCreate {
                repository: repository.full_name(),
                source,
            },
            _ => NewProjectError::GitHub(source),
        }
    })
}

fn github_repository(resolved: &ResolvedRequest) -> Result<&RepositoryName, NewProjectError> {
    resolved
        .repository
        .as_ref()
        .ok_or(NewProjectError::StepOrder(
            "A GitHub step was planned for a local folder",
        ))
}

fn register(resolved: &ResolvedRequest, store: &Mutex<StateStore>) -> Result<u32, NewProjectError> {
    let default_branch = velocity_git::current_branch(&resolved.path)?;
    let record = NewProjectRecord {
        name: resolved.name.clone(),
        location_kind: match resolved.repository {
            Some(_) => ProjectLocationKind::GitHub,
            None => ProjectLocationKind::Local,
        },
        repository_path: resolved.path_text(),
        default_branch,
        github_remote: resolved.repository.as_ref().map(RepositoryName::full_name),
    };
    Ok(lock(store, "Persisted state store")?.register_project(record))
}

/// Switches the way the switcher does, saves, and closes the dialog. As in the
/// switcher's command, the store is locked before the layout and released before
/// `publish` runs.
fn finish(
    store: &Mutex<StateStore>,
    layout: &Mutex<LayoutState>,
    identifier: u32,
    switch: bool,
    publish: impl FnOnce(Publication) -> Result<(), String>,
) -> Result<(), NewProjectError> {
    let mut store = lock(store, "Persisted state store")?;
    let mut layout = lock(layout, "Main-window state")?;
    let selection = if switch {
        Some(store.switch_project_layout(&mut layout, identifier)?)
    } else {
        None
    };
    // The switch writes only when the active project changes; this covers the rest.
    store.save_if_changed()?;
    layout.close_new_project();
    let projects = store.list_projects();
    drop(store);
    publish(Publication {
        snapshot: layout.snapshot(),
        selection: selection.map(|selection| (projects, selection)),
    })
    .map_err(NewProjectError::Publish)
}

fn lock<'a, Value>(
    mutex: &'a Mutex<Value>,
    name: &'static str,
) -> Result<MutexGuard<'a, Value>, NewProjectError> {
    mutex.lock().map_err(|_| NewProjectError::Poisoned(name))
}
