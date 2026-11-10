use crate::state::ManagedStateStore;
use std::sync::Mutex;
use tauri::{Emitter, Manager, State, Window, Wry};
use tauri_specta::Event;
use velocity_acp::{available_commands, client::AgentEvent};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};
use velocity_core::navigation::NavigationIntent;
use velocity_core::thread_view_model::AgentCommandView;
use velocity_interview::Interviews;
use velocity_store::build_gate;

pub type MainLayout = Mutex<LayoutState>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "layout://state")]
pub struct LayoutStateChanged {
    pub state: LayoutSnapshot,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "layout://intent")]
pub struct LayoutIntentRequested {
    pub intent: LayoutIntent,
}

#[tauri::command]
#[specta::specta]
pub fn get_layout_state(
    store: State<'_, ManagedStateStore>,
    state: State<'_, MainLayout>,
) -> Result<LayoutSnapshot, String> {
    let store = store.lock().map_err(|error| error.to_string())?;
    let mut state = state
        .lock()
        .map_err(|error| format!("Could not read main-window state: {error}"))?;
    Ok(build_gate::layout_snapshot(store.state(), &mut state))
}

#[tauri::command]
#[specta::specta]
pub fn dispatch_layout(
    window: Window,
    store: State<'_, ManagedStateStore>,
    state: State<'_, MainLayout>,
    action: LayoutAction,
) -> Result<LayoutSnapshot, String> {
    let mut store = store.lock().map_err(|error| error.to_string())?;
    let mut state = state
        .lock()
        .map_err(|error| format!("Could not update main-window state: {error}"))?;
    let outcome = store
        .dispatch_project_layout(&mut state, action)
        .map_err(|error| error.to_string())?;
    if outcome.selection.is_some() {
        // A project switch rebuilds the window from state.json; put live interviews back.
        velocity_interview::overlay_live(window.state::<Interviews>().inner(), &store, &mut state);
    }
    let snapshot = state.snapshot();
    let project_selection = outcome
        .selection
        .map(|selection| (store.list_projects(), selection));
    drop(state);
    drop(store);
    if let Some((project, task)) = outcome.task_added {
        crate::tasks::record_added(window.app_handle(), project, &task);
    }
    // The interview consumes spec-thread intents; build-thread intents remain queued.
    let intent = outcome
        .intent
        .filter(|intent| !crate::interview::handle_intent(window.app_handle(), intent))
        .filter(|intent| !crate::issue_panel::handle_intent(window.app_handle(), intent));
    publish(&window, &snapshot, intent.clone())?;
    if let Some((role, event)) = outcome.duty_event {
        crate::reviews::instruct(window.app_handle(), role, &event.message);
        crate::sweeps::instruct(window.app_handle(), role, &event.message);
        crate::duty::publish_duty_event(&window, role, event)?;
    }
    if let Some((projects, selection)) = project_selection {
        crate::projects::publish_selection(&window, projects, selection)?;
    }
    // Opening the modal marks the channel read now, not at the watcher's next tick.
    if let Some(LayoutIntent::Navigation(NavigationIntent::Airc)) = &intent {
        crate::threads::report(crate::airc_modal::refresh(window.app_handle()));
    }
    // Starting or steering a thread publishes its own newer snapshot.
    match intent {
        Some(intent) => crate::threads::handle_intent(window.app_handle(), &intent)
            .map(|updated| updated.unwrap_or(snapshot)),
        None => Ok(snapshot),
    }
}

pub fn publish<Handle: Emitter<Wry> + Manager<Wry>>(
    window: &Handle,
    snapshot: &LayoutSnapshot,
    intent: Option<LayoutIntent>,
) -> Result<(), String> {
    // Publish under the same lock as the update, preserving reducer/event order.
    LayoutStateChanged {
        state: snapshot.clone(),
    }
    .emit(window)
    .map_err(|error| format!("Could not publish main-window state: {error}"))?;
    if let Some(intent) = intent {
        LayoutIntentRequested { intent }
            .emit(window)
            .map_err(|error| format!("Could not publish navigation intent: {error}"))?;
    }
    Ok(())
}

/// Routes a live ACP `AgentEvent` (`available_commands_update`, `SPEC.md §3.2`) into the thread's
/// slash menu read model and returns the updated `LayoutSnapshot` when the state changed.
pub fn record_agent_event(
    state: &mut LayoutState,
    thread_identifier: usize,
    event: &AgentEvent,
) -> Option<LayoutSnapshot> {
    if let AgentEvent::SessionUpdate(update) = event
        && let Some(commands) = available_commands(update)
    {
        let mapped = commands
            .into_iter()
            .map(|command| AgentCommandView {
                name: command.name,
                description: command.description,
            })
            .collect();
        state.set_thread_agent_commands(thread_identifier, mapped);
        return Some(state.snapshot());
    }
    None
}

/// Applies a live ACP `AgentEvent` to the managed layout state and publishes `layout://state`.
pub fn apply_agent_event(
    window: &Window,
    state: &MainLayout,
    thread_identifier: usize,
    event: &AgentEvent,
) -> Result<Option<LayoutSnapshot>, String> {
    let mut state = state
        .lock()
        .map_err(|error| format!("Could not update main-window state: {error}"))?;
    let Some(snapshot) = record_agent_event(&mut state, thread_identifier, event) else {
        return Ok(None);
    };
    publish(window, &snapshot, None)?;
    Ok(Some(snapshot))
}
