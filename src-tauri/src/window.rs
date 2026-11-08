use anyhow::Context;
use tauri::{Window, WindowEvent};
use tauri_specta::Event;
use velocity_core::window::{WindowAction, WindowOperation, window_operation};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "window://state")]
pub struct WindowStateChanged {
    pub maximized: bool,
}

fn snapshot(window: &Window) -> anyhow::Result<WindowStateChanged> {
    Ok(WindowStateChanged {
        maximized: window
            .is_maximized()
            .context("Could not read window state")?,
    })
}

fn apply(window: &Window, action: WindowAction) -> anyhow::Result<()> {
    let maximized = match action {
        WindowAction::ToggleMaximize => window.is_maximized()?,
        _ => false,
    };
    match window_operation(action, maximized) {
        WindowOperation::Minimize => window.minimize(),
        WindowOperation::Maximize => window.maximize(),
        WindowOperation::Restore => window.unmaximize(),
        WindowOperation::Close => window.close(),
    }
    .context("Could not apply the window control")
}

#[tauri::command]
#[specta::specta]
pub fn get_window_state(window: Window) -> Result<WindowStateChanged, String> {
    snapshot(&window).map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn minimize_window(window: Window) -> Result<(), String> {
    apply(&window, WindowAction::Minimize).map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn toggle_maximize_window(window: Window) -> Result<(), String> {
    apply(&window, WindowAction::ToggleMaximize).map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn close_window(window: Window) -> Result<(), String> {
    apply(&window, WindowAction::Close).map_err(|error| error.to_string())
}

pub fn on_window_event(window: &Window, event: &WindowEvent) {
    if matches!(event, WindowEvent::Resized(_) | WindowEvent::Focused(_)) {
        let result = snapshot(window).and_then(|state| state.emit(window).map_err(Into::into));
        if let Err(error) = result {
            eprintln!("Could not publish native window state: {error}");
        }
    }
}
