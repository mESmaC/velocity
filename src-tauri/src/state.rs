use anyhow::Context;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Manager, State};
use velocity_store::{Settings, StateStore};

pub type ManagedStateStore = Mutex<StateStore>;

fn resolve_store_directory(application: &AppHandle) -> anyhow::Result<PathBuf> {
    application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory")
}

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let directory = resolve_store_directory(application)?;
    let mut store =
        StateStore::load_or_default(directory).context("Could not initialize persisted state")?;
    if let Some(corrupt_path) = store.recovered_corrupt_snapshot() {
        eprintln!(
            "Recovered from corrupt persisted state; moved unreadable file to {}",
            corrupt_path.display()
        );
    }
    store.reset_disconnected_threads()?;
    application
        .state::<crate::threads::ManagedThreads>()
        .lock()
        .map_err(|_| anyhow::anyhow!("Thread sessions lock is poisoned"))?
        .restore(store.state());
    application.manage(Mutex::new(store));
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn get_settings(store: State<'_, ManagedStateStore>) -> Result<Settings, String> {
    let guard = store
        .lock()
        .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
    Ok(guard.settings())
}

#[tauri::command]
#[specta::specta]
pub fn set_settings(
    store: State<'_, ManagedStateStore>,
    settings: Settings,
) -> Result<Settings, String> {
    let mut guard = store
        .lock()
        .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
    guard
        .set_settings(settings)
        .map_err(|error| error.to_string())
}
