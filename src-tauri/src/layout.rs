use std::sync::Mutex;
use tauri::{State, Window};
use tauri_specta::Event;
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};

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
pub fn get_layout_state(state: State<'_, MainLayout>) -> Result<LayoutSnapshot, String> {
    state
        .lock()
        .map(|state| state.snapshot())
        .map_err(|error| format!("Could not read main-window state: {error}"))
}

#[tauri::command]
#[specta::specta]
pub fn dispatch_layout(
    window: Window,
    state: State<'_, MainLayout>,
    action: LayoutAction,
) -> Result<LayoutSnapshot, String> {
    let mut state = state
        .lock()
        .map_err(|error| format!("Could not update main-window state: {error}"))?;
    let intent = state.apply(action);
    let snapshot = state.snapshot();
    // Publish under the same lock as the update, preserving reducer/event order.
    LayoutStateChanged {
        state: snapshot.clone(),
    }
    .emit(&window)
    .map_err(|error| format!("Could not publish main-window state: {error}"))?;
    if let Some(intent) = intent {
        LayoutIntentRequested { intent }
            .emit(&window)
            .map_err(|error| format!("Could not publish navigation intent: {error}"))?;
    }
    Ok(snapshot)
}
