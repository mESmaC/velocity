//! Tauri commands and activity event emission for duty agents (`SPEC.md §3.1`, `§4 F7`, `§7 M5`).

use std::path::{Path, PathBuf};

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::state::ManagedStateStore;
use tauri::{AppHandle, Manager, State, Window};
use tauri_specta::Event;
use velocity_core::duty::{DutyFeedEvent, DutyRole};
use velocity_core::layout_view::LayoutSnapshot;
use velocity_store::duty::{
    DismissDutyAgentRequest, DutyDialogDefaults, DutyMutationOutcome, PauseDutyRequest,
    StartDutyRequest, StopDutyRequest, clock_timestamp,
};
use velocity_threads::{MainWatches, PrDutyReviews, ScoutSweeps};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "duty://event")]
pub struct DutyEventPublished {
    pub role: DutyRole,
    pub event: DutyFeedEvent,
}

impl DutyEventPublished {
    pub fn new(role: DutyRole, event: DutyFeedEvent) -> Self {
        Self { role, event }
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_duty_dialog_defaults(
    store: State<'_, ManagedStateStore>,
) -> Result<DutyDialogDefaults, String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    Ok(store.duty_dialog_defaults())
}

#[tauri::command]
#[specta::specta]
pub fn start_duty(
    window: Window,
    store: State<'_, ManagedStateStore>,
    state: State<'_, MainLayout>,
    request: StartDutyRequest,
) -> Result<LayoutSnapshot, String> {
    apply_start_or_add(&window, &store, &state, request)
}

#[tauri::command]
#[specta::specta]
pub fn add_duty_agents(
    window: Window,
    store: State<'_, ManagedStateStore>,
    state: State<'_, MainLayout>,
    request: StartDutyRequest,
) -> Result<LayoutSnapshot, String> {
    apply_start_or_add(&window, &store, &state, request)
}

#[tauri::command]
#[specta::specta]
pub fn pause_duty(
    window: Window,
    store: State<'_, ManagedStateStore>,
    state: State<'_, MainLayout>,
    request: PauseDutyRequest,
) -> Result<LayoutSnapshot, String> {
    let outcome = {
        let mut store = store.lock().map_err(|error| error.to_string())?;
        let mut state = state
            .lock()
            .map_err(|error| format!("Could not update main-window state: {error}"))?;
        store
            .toggle_pause_duty(&mut state, request, clock_timestamp())
            .map_err(|error| error.to_string())?
    };
    let snapshot = publish_duty_outcome(&window, outcome);
    crate::reviews::wake(window.app_handle());
    crate::sweeps::wake(window.app_handle());
    crate::main_watch::wake(window.app_handle());
    snapshot
}

/// Dismisses one duty agent (F7): it leaves the saved roster, its session stops, and a PR it
/// held is released for the next agent.
#[tauri::command]
#[specta::specta]
pub fn dismiss_duty_agent(
    window: Window,
    store: State<'_, ManagedStateStore>,
    state: State<'_, MainLayout>,
    request: DismissDutyAgentRequest,
) -> Result<LayoutSnapshot, String> {
    let application = window.app_handle();
    let (outcome, project, repository) = {
        let mut store = store.lock().map_err(|error| error.to_string())?;
        let (project, repository) = active_project(&store)?;
        let held = application
            .try_state::<PrDutyReviews>()
            .filter(|_| request.role == DutyRole::PrDuty)
            .and_then(|reviews| reviews.held(project, &request.nickname));
        let mut state = state
            .lock()
            .map_err(|error| format!("Could not update main-window state: {error}"))?;
        let outcome = store
            .dismiss_duty_agent(&mut state, &request, held, clock_timestamp())
            .map_err(|error| error.to_string())?;
        (outcome, project, repository)
    };
    let nicknames = std::slice::from_ref(&request.nickname);
    stop_sessions(application, request.role, project, &repository, nicknames);
    publish_duty_outcome(&window, outcome)
}

/// Stops a duty role (F7): every agent on it is dismissed and the role leaves "On duty".
#[tauri::command]
#[specta::specta]
pub fn stop_duty(
    window: Window,
    store: State<'_, ManagedStateStore>,
    state: State<'_, MainLayout>,
    request: StopDutyRequest,
) -> Result<LayoutSnapshot, String> {
    let (stopped, project, repository) = {
        let mut store = store.lock().map_err(|error| error.to_string())?;
        let (project, repository) = active_project(&store)?;
        let mut state = state
            .lock()
            .map_err(|error| format!("Could not update main-window state: {error}"))?;
        let stopped = store
            .stop_duty(&mut state, &request, &clock_timestamp())
            .map_err(|error| error.to_string())?;
        (stopped, project, repository)
    };
    let application = window.app_handle();
    stop_sessions(
        application,
        request.role,
        project,
        &repository,
        &stopped.nicknames,
    );
    publish_duty_outcome(&window, stopped.outcome)
}

fn active_project(store: &velocity_store::StateStore) -> Result<(u32, PathBuf), String> {
    store
        .active_project()
        .map(|project| (project.id, PathBuf::from(&project.repository_path)))
        .ok_or_else(|| "No project is open".to_owned())
}

/// Stops the dismissed agents' running sessions through their role's runner. They already
/// left the saved roster, so no runner starts them again.
fn stop_sessions(
    application: &AppHandle,
    role: DutyRole,
    project: u32,
    repository: &Path,
    nicknames: &[String],
) {
    for nickname in nicknames {
        match role {
            DutyRole::PrDuty => {
                if let Some(reviews) = application.try_state::<PrDutyReviews>() {
                    reviews.dismiss(project, nickname, repository);
                }
            }
            DutyRole::Scouts => {
                if let Some(sweeps) = application.try_state::<ScoutSweeps>() {
                    sweeps.dismiss(project, nickname);
                }
            }
            DutyRole::MainWatcher => {
                if let Some(watches) = application.try_state::<MainWatches>() {
                    watches.dismiss(project, nickname);
                }
            }
        }
    }
}

fn apply_start_or_add(
    window: &Window,
    store: &State<'_, ManagedStateStore>,
    state: &State<'_, MainLayout>,
    request: StartDutyRequest,
) -> Result<LayoutSnapshot, String> {
    let outcome = {
        let mut store = store.lock().map_err(|error| error.to_string())?;
        let mut state = state
            .lock()
            .map_err(|error| format!("Could not update main-window state: {error}"))?;
        store
            .start_or_add_duty(&mut state, request, clock_timestamp())
            .map_err(|error| error.to_string())?
    };
    let snapshot = publish_duty_outcome(window, outcome);
    // New scouts sweep at once, and new PR duty agents claim PRs at once (F7).
    crate::reviews::wake(window.app_handle());
    crate::sweeps::wake(window.app_handle());
    crate::main_watch::wake(window.app_handle());
    snapshot
}

fn publish_duty_outcome(
    window: &Window,
    outcome: DutyMutationOutcome,
) -> Result<LayoutSnapshot, String> {
    LayoutStateChanged {
        state: outcome.snapshot.clone(),
    }
    .emit(window)
    .map_err(|error| format!("Could not publish main-window state: {error}"))?;
    publish_duty_event(window, outcome.role, outcome.event)?;
    Ok(outcome.snapshot)
}

pub fn publish_duty_event(
    window: &Window,
    role: DutyRole,
    event: DutyFeedEvent,
) -> Result<(), String> {
    DutyEventPublished { role, event }
        .emit(window)
        .map_err(|error| format!("Could not publish duty event: {error}"))
}
