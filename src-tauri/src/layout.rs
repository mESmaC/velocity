use crate::state::ManagedStateStore;
use std::sync::Mutex;
use tauri::{State, Window};
use tauri_specta::Event;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};
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
    let project_selection = outcome
        .selection
        .map(|selection| (store.list_projects(), selection));
    drop(store);
    publish(&window, &outcome.snapshot, outcome.intent)?;
    if let Some((projects, selection)) = project_selection {
        crate::projects::publish_selection(&window, projects, selection)?;
    }
    Ok(outcome.snapshot)
}

pub fn publish(
    window: &Window,
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
