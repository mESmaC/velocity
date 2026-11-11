//! Runs Create: observe, plan, run each step, then save, switch and close.

use std::sync::{Mutex, MutexGuard};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::new_project::{
    NewProjectLocation, Observation, Refusal, SetupStep, plan_new_project,
};
use velocity_github::RepositoryName;
use velocity_store::projects::{ProjectList, ProjectSelectionView};
use velocity_store::{NewProjectRecord, PendingInterview, StateStore};

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
    spec_locked: bool,
}

/// The spec every project keeps at its root (`ProjectItem::spec_path`'s default).
const SPEC_FILE: &str = "SPEC.md";

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
    let settings = lock(store, "Persisted state store")?.settings();
    // The interview runs on the provider new work starts on: refuse before anything is
    // created when you are signed out of every provider (F15 Sign out).
    settings.ensure_new_work(settings.new_work_choice())?;
    let interview = PendingInterview {
        model_name: request.model_name.clone(),
        effort: request.effort,
        preferred_speed: request.speed,
        brief: request.brief.trim().to_owned(),
    };
    set_up(&resolved, interview, store, layout, publish)
}

/// Observes, plans and runs every step for a resolved location, then saves the
/// project and switches to it. Shared by Create and Open project.
pub(crate) fn set_up(
    resolved: &ResolvedRequest,
    interview: PendingInterview,
    store: &Mutex<StateStore>,
    layout: &Mutex<LayoutState>,
    publish: impl FnOnce(Publication) -> Result<(), String>,
) -> Result<CreatedProject, NewProjectError> {
    let already_a_project =
        lock(store, "Persisted state store")?.has_project_at(&resolved.path_text());
    let observation = observe(resolved, already_a_project)?;
    let steps = plan_new_project(resolved.location, observation)
        .map_err(|refusal| NewProjectError::Refused(refusal_message(refusal, resolved)))?;
    let mut pending = PendingProject::default();
    for step in steps {
        run_step(step, resolved, &interview, &mut pending)?;
    }
    let installed =
        velocity_git::agents_template::install_default_agents(&resolved.path, &resolved.name)?;
    // A failed commit leaves the template on disk. Retry it before registration so
    // new worktrees inherit the instructions, without adopting a user's custom file.
    if installed
        || velocity_git::agents_template::is_default_agents(&resolved.path, &resolved.name)?
    {
        velocity_git::agents_file::commit_agents(&resolved.path)?;
    }
    let identifier = finish(resolved, pending, store, layout, publish)?;
    Ok(CreatedProject {
        identifier,
        name: resolved.name.clone(),
        repository_path: resolved.path_text(),
    })
}

/// Reads the facts the planner decides from. GitHub is asked only about GitHub
/// locations, and not at all when Open project finds your clone already there.
fn observe(
    resolved: &ResolvedRequest,
    already_a_project: bool,
) -> Result<Observation, NewProjectError> {
    let is_repository = velocity_git::is_repository(&resolved.path);
    let target_is_checkout = match &resolved.repository {
        Some(repository) if is_repository => velocity_github::checkout_repository(&resolved.path)
            .is_some_and(|origin| same_repository(&origin, repository)),
        _ => false,
    };
    let (remote_exists, target_occupied) = match &resolved.repository {
        Some(_) if target_is_checkout => (true, true),
        Some(repository) => (
            velocity_github::repository_exists(repository)?,
            velocity_git::folder_is_occupied(&resolved.path)?,
        ),
        None => (false, false),
    };
    Ok(Observation {
        already_a_project,
        is_repository,
        remote_exists,
        target_occupied,
        target_is_checkout,
    })
}

/// GitHub owner and repository names ignore case.
fn same_repository(left: &RepositoryName, right: &RepositoryName) -> bool {
    left.full_name().eq_ignore_ascii_case(&right.full_name())
}

fn run_step(
    step: SetupStep,
    resolved: &ResolvedRequest,
    interview: &PendingInterview,
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
        SetupStep::StartInterview => pending.interview = Some(interview.clone()),
        // Rule 2 holds for opened projects too: a project without a spec gets its
        // interview, and only an existing SPEC.md counts as a locked spec.
        SetupStep::AdoptSpec => {
            if resolved.path.join(SPEC_FILE).is_file() {
                pending.spec_locked = true;
            } else {
                pending.interview = Some(interview.clone());
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

fn project_record(resolved: &ResolvedRequest) -> Result<NewProjectRecord, NewProjectError> {
    let default_branch = if resolved.location.opens_existing() {
        velocity_git::opened_project_default_branch(&resolved.path)?
    } else {
        velocity_git::current_branch(&resolved.path)?
    };
    let record = NewProjectRecord::new(resolved.name.clone(), resolved.path_text(), default_branch);
    // An opened folder whose origin is on GitHub is a GitHub project, as a clone is.
    let repository = resolved.repository.clone().or_else(|| {
        (resolved.location == NewProjectLocation::OpenFolder)
            .then(|| velocity_github::checkout_repository(&resolved.path))
            .flatten()
    });
    Ok(match repository {
        Some(repository) => record.on_github(repository.full_name()),
        None => record,
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
    let record = pending
        .record
        .ok_or(NewProjectError::StepOrder(
            "Create finished without registering the project",
        ))?
        .with_spec_locked(pending.spec_locked);
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
