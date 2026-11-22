use anyhow::Context;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State, Webview, Window, WindowEvent};
use tauri_specta::Event;
use velocity_core::window::{WindowAction, WindowOperation, window_operation};

pub const MAIN_WINDOW: &str = "main";
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);
const PAGE_START_GRACE: Duration = Duration::from_secs(12);
/// How long a freshly restarted local build has to report its first visible renderer
/// heartbeat before startup is considered failed (#609).
const STARTUP_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

struct HeartbeatState {
    deadline: Instant,
    received_any: bool,
    consecutive_timeouts: u32,
    reload_failed: bool,
}

pub struct WindowHeartbeat(Mutex<HeartbeatState>);

impl Default for WindowHeartbeat {
    fn default() -> Self {
        Self(Mutex::new(HeartbeatState {
            deadline: Instant::now() + PAGE_START_GRACE,
            received_any: false,
            consecutive_timeouts: 0,
            reload_failed: false,
        }))
    }
}

impl WindowHeartbeat {
    fn extend(&self, grace: Duration) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .deadline = Instant::now() + grace;
    }

    fn record_beat(&self) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.deadline = Instant::now() + HEARTBEAT_TIMEOUT;
        state.received_any = true;
        state.consecutive_timeouts = 0;
        state.reload_failed = false;
    }

    fn expired(&self) -> bool {
        Instant::now()
            >= self
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .deadline
    }

    fn begin_recovery(&self) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        if now < state.deadline {
            return false;
        }
        // Give the next page time to boot before trying again.
        state.deadline = now + PAGE_START_GRACE;
        state.consecutive_timeouts = state.consecutive_timeouts.saturating_add(1);
        true
    }

    fn record_reload_failure(&self) {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reload_failed = true;
    }

    /// Returns a failure reason when a freshly restarted local build failed startup or
    /// renderer heartbeat checks (#609).
    pub(crate) fn post_restart_failure(
        &self,
        elapsed: Duration,
        window_visible: bool,
    ) -> Option<&'static str> {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if state.reload_failed {
            return Some("renderer heartbeat timed out and main window reload failed");
        }
        if state.consecutive_timeouts >= 2 {
            return Some("renderer missed heartbeats repeatedly after restart");
        }
        if window_visible && !state.received_any && elapsed >= STARTUP_HEARTBEAT_TIMEOUT {
            return Some("renderer did not report a startup heartbeat");
        }
        None
    }
}

#[tauri::command]
#[specta::specta]
pub fn window_heartbeat(webview: Webview, heartbeat: State<'_, WindowHeartbeat>) {
    if webview.label() == MAIN_WINDOW {
        heartbeat.record_beat();
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
                    heartbeat.record_reload_failure();
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
    if window.label() == MAIN_WINDOW && matches!(event, WindowEvent::Focused(true)) {
        crate::collapse_mirror::on_window_focused(window.app_handle());
    }
}
