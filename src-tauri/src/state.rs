use anyhow::Context;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use velocity_core::state::ui_zoom::{UiZoomAdjustment, UiZoomView};
use velocity_store::{Settings, StateStore, StoreLock};

pub type ManagedStateStore = StoreLock;

fn resolve_store_directory(application: &AppHandle) -> anyhow::Result<PathBuf> {
    application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory")
}

/// Loads `state.json` and returns the build threads whose turn the last exit cut off, which
/// `crate::restart` continues once the rest of the app is up (§3.2).
pub fn initialize(application: &AppHandle) -> anyhow::Result<Vec<velocity_threads::ThreadKey>> {
    let directory = resolve_store_directory(application)?;
    let mut store =
        StateStore::load_or_default(directory).context("Could not initialize persisted state")?;
    if let Some(corrupt_path) = store.recovered_corrupt_snapshot() {
        eprintln!(
            "Recovered from corrupt persisted state; moved unreadable file to {}",
            corrupt_path.display()
        );
    }
    let interrupted = velocity_threads::restart::interrupted_turns(store.state());
    // Persist before resetting activity, so a second startup cannot forget recovery.
    store.reserve_startup_recovery(&interrupted)?;
    store.reset_disconnected_threads()?;
    application
        .state::<crate::threads::ManagedThreads>()
        .lock()
        .map_err(|_| anyhow::anyhow!("Thread sessions lock is poisoned"))?
        .restore(store.state());
    store.defer_saves();
    application.manage(ManagedStateStore::new(store));
    start_background_saves(application);
    Ok(interrupted)
}

/// How often the background writer saves the latest change to `state.json`. A crash loses
/// at most this much; an orderly exit saves first (`save_now`).
const SAVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Saves `state.json` behind the store lock (#285, #320): changes only update memory, and
/// this loop writes the newest one at most once a second.
fn start_background_saves(application: &AppHandle) {
    let application = application.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(SAVE_INTERVAL);
            if let Err(error) = save_now(&application) {
                eprintln!("{error}");
            }
        }
    });
}

/// Writes any change not yet in `state.json`. Called by the background loop, and before
/// Velocity exits or hands over to an installer.
pub fn save_now(application: &AppHandle) -> Result<(), String> {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return Ok(());
    };
    velocity_store::save_pending(store.inner().as_mutex())
        .map(|_| ())
        .map_err(|error| format!("Could not save state: {error}"))
}

#[tauri::command]
#[specta::specta]
pub async fn get_settings(application: AppHandle) -> Result<Settings, String> {
    crate::layout::off_ui_thread_named("get_settings", move || {
        let store = application.state::<ManagedStateStore>();
        let guard = store
            .lock()
            .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
        Ok(guard.settings())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_settings(application: AppHandle, settings: Settings) -> Result<Settings, String> {
    crate::layout::off_ui_thread_named("set_settings", move || {
        let store = application.state::<ManagedStateStore>();
        let mut guard = store
            .lock()
            .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
        let saved = guard
            .set_settings(settings)
            .map_err(|error| error.to_string())?;
        application
            .state::<velocity_browser::BrowserHub>()
            .limit(saved.agents.browser_limit);
        Ok(saved)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_ui_zoom(application: AppHandle) -> Result<UiZoomView, String> {
    let handle = application.clone();
    let view = crate::layout::off_ui_thread_named("get_ui_zoom", move || {
        let store = handle.state::<ManagedStateStore>();
        let store = store.lock().map_err(|error| error.to_string())?;
        Ok(store.state().settings.ui_zoom.view())
    })
    .await?;
    apply_ui_zoom(&application, &view);
    Ok(view)
}

#[tauri::command]
#[specta::specta]
pub async fn adjust_ui_zoom(
    application: AppHandle,
    adjustment: UiZoomAdjustment,
) -> Result<UiZoomView, String> {
    let handle = application.clone();
    let view = crate::layout::off_ui_thread_named("adjust_ui_zoom", move || {
        let store = handle.state::<ManagedStateStore>();
        let mut store = store.lock().map_err(|error| error.to_string())?;
        store
            .adjust_ui_zoom(adjustment)
            .map_err(|error| error.to_string())
    })
    .await?;
    apply_ui_zoom(&application, &view);
    Ok(view)
}

/// Zooms the whole webview, as a browser's page zoom does. Scaling only the root font
/// size left every pixel length (padding, icons, pane widths) at full size while the
/// text shrank inside it (#495).
fn apply_ui_zoom(application: &AppHandle, view: &UiZoomView) {
    let Some(window) = application.get_webview_window(crate::window::MAIN_WINDOW) else {
        return;
    };
    if let Err(error) = window.set_zoom(f64::from(view.percent) / 100.0) {
        eprintln!("Velocity could not apply the UI zoom: {error}");
    }
}
