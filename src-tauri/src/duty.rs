//! Tauri commands and activity event emission for duty agents (`SPEC.md §3.1`, `§4 F7`, `§7 M5`).

use crate::layout::{LayoutStateChanged, MainLayout};
use crate::state::ManagedStateStore;
use tauri::{Manager, State, Window};
use tauri_specta::Event;
use velocity_core::duty::{DutyFeedEvent, DutyRole};
use velocity_core::layout_view::LayoutSnapshot;
use velocity_store::duty::{
    DutyDialogDefaults, DutyMutationOutcome, PauseDutyRequest, StartDutyRequest, clock_timestamp,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "duty://event")]
pub struct DutyEventPublished {
    pub role: DutyRole,
    pub event: DutyFeedEvent,
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
    snapshot
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
