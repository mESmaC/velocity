use anyhow::Context;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State, Webview, Window, WindowEvent};
use tauri_specta::Event;
use velocity_core::window::{WindowAction, WindowOperation, window_operation};

const MAIN_WINDOW: &str = "main";
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);
const PAGE_START_GRACE: Duration = Duration::from_secs(12);

pub struct WindowHeartbeat(Mutex<Instant>);

impl Default for WindowHeartbeat {
    fn default() -> Self {
        Self(Mutex::new(Instant::now() + PAGE_START_GRACE))
    }
}

impl WindowHeartbeat {
    fn extend(&self, grace: Duration) {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) = Instant::now() + grace;
    }

    fn expired(&self) -> bool {
        Instant::now() >= *self.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn begin_recovery(&self) -> bool {
        let mut deadline = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        if now < *deadline {
            return false;
        }
        // Give the next page time to boot before trying again.
        *deadline = now + PAGE_START_GRACE;
        true
    }
}

#[tauri::command]
#[specta::specta]
pub fn window_heartbeat(webview: Webview, heartbeat: State<'_, WindowHeartbeat>) {
    if webview.label() == MAIN_WINDOW {
        heartbeat.extend(HEARTBEAT_TIMEOUT);
    }
}

pub fn watch_renderer(application: &AppHandle) {
    application
        .state::<WindowHeartbeat>()
        .extend(PAGE_START_GRACE);
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Some(window) = application.get_webview_window(MAIN_WINDOW) else {
                break;
            };
            let heartbeat = application.state::<WindowHeartbeat>();
            if !heartbeat.expired() {
                continue;
            }
            // Browsers may throttle timers while the window is minimized or hidden.
            if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
                heartbeat.extend(HEARTBEAT_TIMEOUT);
                continue;
            }
            if heartbeat.begin_recovery() {
                crate::diagnostics::record(
                    "webview",
                    "Heartbeat timed out. Reloading the main window.",
                );
                if let Err(error) = window.reload() {
                    crate::diagnostics::record("webview", &format!("Could not reload: {error}"));
                }
            }
        }
    });
}

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
