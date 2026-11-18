//! Runs Create: observe, plan, run each step, then save, switch and close.

use std::sync::{Mutex, MutexGuard};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::new_project::{NewProjectLocation, Observation, should_clone};
use velocity_rosen::project;
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

// Keep retries and simultaneous opens of the same folder from racing the initial import.
static SETUP: Mutex<()> = Mutex::new(());

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
    if project::is_workspace(&resolved.path) {
        return Err(NewProjectError::Refused(
            "Choose the project folder instead of a Rosen workspace.".into(),
        ));
    }
    let _setup = lock(&SETUP, "Rosen project setup")?;
    let already_a_project =
        lock(store, "Persisted state store")?.has_project_at(&resolved.path_text());
    let observation = observe(resolved, already_a_project)?;
    let clone = should_clone(resolved.location, observation)
        .map_err(|refusal| NewProjectError::Refused(refusal_message(refusal, resolved)))?;
    if clone && let Some(repository) = &resolved.repository {
        velocity_github::clone_repository(repository, &resolved.path)?;
    }
    let runtime = tokio::runtime::Runtime::new().map_err(project::ProjectError::from)?;
    let base = runtime.block_on(project::prepare(
        &resolved.path,
        &resolved.name,
        resolved.mirror.as_deref(),
        resolved.location.opens_existing(),
    ))?;
    let record = project_record(resolved, base.branch).with_spec_locked(base.has_spec);
    let mut store = lock(store, "Persisted state store")?;
    project::link(
        store.data_directory(),
        store.next_project_identifier(),
        &resolved.path,
    )?;
    let mut layout = lock(layout, "Main-window state")?;
    let identifier =
        store.commit_new_project(record, (!base.has_spec).then_some(interview), true)?;
    let selection = store.switch_project_layout(&mut layout, identifier)?;
    layout.close_new_project();
    let projects = store.list_projects();
    drop(store);
    publish(Publication {
        snapshot: layout.snapshot(),
        selection: Some((projects, selection)),
    })
    .map_err(NewProjectError::Publish)?;
    Ok(CreatedProject {
        identifier,
        name: resolved.name.clone(),
        repository_path: resolved.path_text(),
    })
}

fn observe(
    resolved: &ResolvedRequest,
    already_a_project: bool,
) -> Result<Observation, NewProjectError> {
    let mut observation = Observation {
        already_a_project,
        is_repository: false,
        remote_exists: false,
        target_occupied: false,
        target_is_checkout: false,
    };
    if already_a_project {
        return Ok(observation);
    }
    observation.is_repository =
        project::has_local_server(&resolved.path) || velocity_git::is_repository(&resolved.path);
    if let Some(repository) = &resolved.repository {
        observation.target_is_checkout = observation.is_repository
            && velocity_github::checkout_repository(&resolved.path).is_some_and(|origin| {
                origin
                    .full_name()
                    .eq_ignore_ascii_case(&repository.full_name())
            });
        if !observation.target_is_checkout {
            observation.target_occupied = velocity_git::folder_is_occupied(&resolved.path)?;
            if !observation.target_occupied {
                observation.remote_exists = velocity_github::repository_exists(repository)?;
            }
        }
    }
    Ok(observation)
}

fn project_record(resolved: &ResolvedRequest, branch: String) -> NewProjectRecord {
    let record = NewProjectRecord::new(resolved.name.clone(), resolved.path_text(), branch);
    let github_mirror = resolved
        .mirror
        .as_deref()
        .filter(|mirror| mirror.starts_with("https://github.com/"))
        .and_then(|mirror| velocity_github::RepositoryName::parse(mirror).ok());
    let repository = resolved.repository.clone().or(github_mirror).or_else(|| {
        (resolved.location == NewProjectLocation::OpenFolder)
            .then(|| velocity_github::checkout_repository(&resolved.path))
            .flatten()
    });
    match repository {
        Some(repository) => record.on_github(repository.full_name()),
        None => record,
    }
}

fn lock<'a, Value>(
    mutex: &'a Mutex<Value>,
    name: &'static str,
) -> Result<MutexGuard<'a, Value>, NewProjectError> {
    mutex.lock().map_err(|_| NewProjectError::Poisoned(name))
}
