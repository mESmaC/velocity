//! Runs Create: observe, plan, run each step, then save, switch and close.

use std::sync::{Mutex, MutexGuard};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::new_project::{
    NewProjectLocation, Observation, Refusal, SetupStep, plan_new_project,
};
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

/// What the store steps asked for. Nothing reaches the store until `finish` saves
/// it all at once, so a failed save leaves no project behind and Create can be
/// retried; the repository set up so far is kept.
#[derive(Default)]
struct PendingProject {
    record: Option<NewProjectRecord>,
    interview: Option<PendingInterview>,
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
    let mut pending = PendingProject::default();
    for step in steps {
        run_step(step, &resolved, request, &mut pending)?;
    }
    if velocity_git::agents_template::install_default_agents(&resolved.path, &resolved.name)? {
        velocity_git::commit_file(
            &resolved.path,
            std::path::Path::new("AGENTS.md"),
            "Add default project agent instructions",
        )?;
    }
    let identifier = finish(&resolved, pending, store, layout, publish)?;
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
    pending: &mut PendingProject,
) -> Result<(), NewProjectError> {
    match step {
        SetupStep::GitInit => velocity_git::initialize_repository(&resolved.path)?,
        SetupStep::CreateRemote => {
            velocity_github::create_repository(github_repository(resolved)?, resolved.visibility)?
        }
        SetupStep::Clone => clone(resolved)?,
        SetupStep::Register => pending.record = Some(project_record(resolved)?),
        SetupStep::SwitchTo => pending.switch_requested = true,
        SetupStep::StartInterview => {
            pending.interview = Some(PendingInterview {
                model_name: request.model_name.clone(),
                effort: request.effort,
                preferred_speed: request.speed,
                brief: request.brief.trim().to_owned(),
            });
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

fn project_record(resolved: &ResolvedRequest) -> Result<NewProjectRecord, NewProjectError> {
    Ok(NewProjectRecord {
        name: resolved.name.clone(),
        location_kind: match resolved.repository {
            Some(_) => ProjectLocationKind::GitHub,
            None => ProjectLocationKind::Local,
        },
        repository_path: resolved.path_text(),
        default_branch: velocity_git::current_branch(&resolved.path)?,
        github_remote: resolved.repository.as_ref().map(RepositoryName::full_name),
    })
}

/// Saves the project, switches to it the way the switcher does, and closes the
/// dialog. As in the switcher's command, the store is locked before the layout and
/// released before `publish` runs.
fn finish(
    resolved: &ResolvedRequest,
    pending: PendingProject,
    store: &Mutex<StateStore>,
    layout: &Mutex<LayoutState>,
    publish: impl FnOnce(Publication) -> Result<(), String>,
) -> Result<u32, NewProjectError> {
    let record = pending.record.ok_or(NewProjectError::StepOrder(
        "Create finished without registering the project",
    ))?;
    let mut store = lock(store, "Persisted state store")?;
    // Another Create may have registered the same folder while git or gh ran.
    if store.has_project_at(&record.repository_path) {
        let message = refusal_message(Refusal::AlreadyAProject, resolved);
        return Err(NewProjectError::Refused(message));
    }
    let mut layout = lock(layout, "Main-window state")?;
    let identifier =
        store.commit_new_project(record, pending.interview, pending.switch_requested)?;
    // The project is already saved as active, so this only refreshes the layout.
    let selection = if pending.switch_requested {
        Some(store.switch_project_layout(&mut layout, identifier)?)
    } else {
        None
    };
    layout.close_new_project();
    let projects = store.list_projects();
    drop(store);
    publish(Publication {
        snapshot: layout.snapshot(),
        selection: selection.map(|selection| (projects, selection)),
    })
    .map_err(NewProjectError::Publish)?;
    Ok(identifier)
}

fn lock<'a, Value>(
    mutex: &'a Mutex<Value>,
    name: &'static str,
) -> Result<MutexGuard<'a, Value>, NewProjectError> {
    mutex.lock().map_err(|_| NewProjectError::Poisoned(name))
}
