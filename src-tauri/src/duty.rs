//! Tauri commands and activity event emission for duty agents (`SPEC.md §3.1`, `§4 F7`, `§7 M5`).

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::state::ManagedStateStore;
use tauri::{AppHandle, Manager, State, Window};
use tauri_specta::Event;
use velocity_core::duty::{DutyFeedEvent, DutyRole};
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::duty::{
    DismissDutyAgentRequest, DutyDialogDefaults, DutyMutationOutcome, PauseDutyRequest,
    StartDutyRequest, StopDutyRequest, clock_timestamp,
};
use velocity_threads::ScoutSweeps;

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
pub async fn get_duty_dialog_defaults(
    application: AppHandle,
) -> Result<DutyDialogDefaults, String> {
    let reader = application.clone();
    let (mut defaults, project) = crate::layout::off_ui_thread(move || {
        let store = reader.state::<ManagedStateStore>();
        let store = store.lock().map_err(|error| error.to_string())?;
        let project = store
            .active_project()
            .filter(|project| project.rosen_in_use())
            .map(|project| project.id);
        Ok((store.duty_dialog_defaults(), project))
    })
    .await?;
    if let (Some(project), Ok(app_data)) = (project, application.path().app_data_dir()) {
        let server = velocity_rosen::ProjectServer::new(&app_data, project);
        defaults.review_swarm = velocity_rosen::post_land::after_land(&server)
            .await
            .unwrap_or(false);
    }
    Ok(defaults)
}

/// A duty agent's review transcript (F7), including session boundaries for this app run.
#[tauri::command]
#[specta::specta]
pub async fn get_duty_agent_transcript(
    window: Window,
    nickname: String,
) -> Result<Option<Vec<TranscriptBlock>>, String> {
    crate::layout::off_ui_thread(move || {
        let store = window.state::<ManagedStateStore>();
        let project = {
            let store = store.lock().map_err(|error| error.to_string())?;
            active_project(&store)?
        };
        Ok(window
            .app_handle()
            .try_state::<ScoutSweeps>()
            .and_then(|sweeps| sweeps.review_transcript(project, &nickname)))
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn start_duty(
    window: Window,
    request: StartDutyRequest,
) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        let store = window.state::<ManagedStateStore>();
        let state = window.state::<MainLayout>();
        apply_start_or_add(&window, &store, &state, request)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn add_duty_agents(
    window: Window,
    request: StartDutyRequest,
) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        let store = window.state::<ManagedStateStore>();
        let state = window.state::<MainLayout>();
        apply_start_or_add(&window, &store, &state, request)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn pause_duty(
    window: Window,
    request: PauseDutyRequest,
) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        let store = window.state::<ManagedStateStore>();
        let state = window.state::<MainLayout>();
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
        crate::sweeps::wake(window.app_handle());
        snapshot
    })
    .await
}

/// Dismisses a Scout or review worker and stops its running session.
#[tauri::command]
#[specta::specta]
pub async fn dismiss_duty_agent(
    window: Window,
    request: DismissDutyAgentRequest,
) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        let store = window.state::<ManagedStateStore>();
        let state = window.state::<MainLayout>();
        let application = window.app_handle();
        let (outcome, project) = {
            let mut store = store.lock().map_err(|error| error.to_string())?;
            let project = active_project(&store)?;
            let mut state = state
                .lock()
                .map_err(|error| format!("Could not update main-window state: {error}"))?;
            let outcome = store
                .dismiss_duty_agent(&mut state, &request, None, clock_timestamp())
                .map_err(|error| error.to_string())?;
            (outcome, project)
        };
        let nicknames = std::slice::from_ref(&request.nickname);
        stop_sessions(application, project, nicknames);
        publish_duty_outcome(&window, outcome)
    })
    .await
}

/// Stops a duty role (F7): every agent on it is dismissed and the role leaves "On duty".
#[tauri::command]
#[specta::specta]
pub async fn stop_duty(window: Window, request: StopDutyRequest) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        let store = window.state::<ManagedStateStore>();
        let state = window.state::<MainLayout>();
        let (stopped, project) = {
            let mut store = store.lock().map_err(|error| error.to_string())?;
            let project = active_project(&store)?;
            let mut state = state
                .lock()
                .map_err(|error| format!("Could not update main-window state: {error}"))?;
            let stopped = store
                .stop_duty(&mut state, &request, &clock_timestamp())
                .map_err(|error| error.to_string())?;
            (stopped, project)
        };
        let application = window.app_handle();
        stop_sessions(application, project, &stopped.nicknames);
        publish_duty_outcome(&window, stopped.outcome)
    })
    .await
}

fn active_project(store: &velocity_store::StateStore) -> Result<u32, String> {
    store
        .active_project()
        .map(|project| project.id)
        .ok_or_else(|| "No project is open".to_owned())
}

/// Stops the dismissed agents' running sessions through their role's runner. They already
/// left the saved roster, so no runner starts them again.
fn stop_sessions(application: &AppHandle, project: u32, nicknames: &[String]) {
    if let Some(sweeps) = application.try_state::<ScoutSweeps>() {
        for nickname in nicknames {
            sweeps.dismiss(project, nickname);
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
    crate::sweeps::wake(window.app_handle());
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
