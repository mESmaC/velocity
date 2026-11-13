use anyhow::Context;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use velocity_store::{Settings, StateStore};

pub type ManagedStateStore = Mutex<StateStore>;

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
    store.reset_disconnected_threads()?;
    application
        .state::<crate::threads::ManagedThreads>()
        .lock()
        .map_err(|_| anyhow::anyhow!("Thread sessions lock is poisoned"))?
        .restore(store.state());
    store.defer_saves();
    application.manage(Mutex::new(store));
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
    velocity_store::save_pending(store.inner())
        .map(|_| ())
        .map_err(|error| format!("Could not save state: {error}"))
}

#[tauri::command]
#[specta::specta]
pub async fn get_settings(application: AppHandle) -> Result<Settings, String> {
    crate::layout::off_ui_thread(move || {
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
    crate::layout::off_ui_thread(move || {
        let store = application.state::<ManagedStateStore>();
        let mut guard = store
            .lock()
            .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
        guard
            .set_settings(settings)
            .map_err(|error| error.to_string())
    })
    .await
}
