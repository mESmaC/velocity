use anyhow::Context;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use velocity_core::state::ui_zoom::{UiZoom, UiZoomAdjustment, UiZoomView};
use velocity_store::{
    AgentSettings, ProofSettings, ProviderSettings, Settings, StateStore, StoreLock,
    WorktreeSettings,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename = "WorkspaceSettings")]
pub struct AppWorkspaceSettings {
    pub location_root: Option<String>,
    #[serde(default)]
    pub delete_local_change_on_land: bool,
    pub share_rust_build_cache: bool,
    pub warm_pool_count: u32,
    #[serde(default)]
    pub project_roots: BTreeMap<u32, String>,
    #[serde(default)]
    pub copy_files: String,
    #[serde(default)]
    pub setup_command: String,
    #[serde(default)]
    pub project_setup_commands: BTreeMap<u32, String>,
}

impl From<WorktreeSettings> for AppWorkspaceSettings {
    fn from(settings: WorktreeSettings) -> Self {
        Self {
            location_root: settings.location_root,
            delete_local_change_on_land: settings.delete_local_branch_on_merge,
            share_rust_build_cache: settings.share_rust_build_cache,
            warm_pool_count: settings.warm_pool_count,
            project_roots: settings.project_roots,
            copy_files: settings.copy_files,
            setup_command: settings.setup_command,
            project_setup_commands: settings.project_setup_commands,
        }
    }
}

impl From<AppWorkspaceSettings> for WorktreeSettings {
    fn from(settings: AppWorkspaceSettings) -> Self {
        Self {
            location_root: settings.location_root,
            delete_local_branch_on_merge: settings.delete_local_change_on_land,
            share_rust_build_cache: settings.share_rust_build_cache,
            warm_pool_count: settings.warm_pool_count,
            project_roots: settings.project_roots,
            copy_files: settings.copy_files,
            setup_command: settings.setup_command,
            project_setup_commands: settings.project_setup_commands,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename = "Settings")]
pub struct AppSettings {
    pub workspaces: AppWorkspaceSettings,
    pub providers: ProviderSettings,
    pub agents: AgentSettings,
    pub proofs: ProofSettings,
    #[serde(default)]
    pub ui_zoom: UiZoom,
}

impl From<Settings> for AppSettings {
    fn from(settings: Settings) -> Self {
        Self {
            workspaces: settings.worktrees.into(),
            providers: settings.providers,
            agents: settings.agents,
            proofs: settings.proofs,
            ui_zoom: settings.ui_zoom,
        }
    }
}

impl From<AppSettings> for Settings {
    fn from(settings: AppSettings) -> Self {
        Self {
            worktrees: settings.workspaces.into(),
            providers: settings.providers,
            agents: settings.agents,
            proofs: settings.proofs,
            ui_zoom: settings.ui_zoom,
        }
    }
}

pub type ManagedStateStore = StoreLock;

/// Runs `work` against the persisted state store on Tokio's blocking pool so async tasks
/// never block a runtime worker on the `std::sync::Mutex`, while keeping the caller's own
/// source location in store-lock diagnostics (#554). Not `async`: `#[track_caller]` does
/// nothing on an async fn, so the caller is read synchronously and the future returned.
#[track_caller]
pub(crate) fn with_store<Value: Send + 'static>(
    application: &AppHandle,
    work: impl FnOnce(&mut StateStore) -> Value + Send + 'static,
) -> impl Future<Output = Result<Value, String>> {
    let caller = std::panic::Location::caller();
    let application = application.clone();
    async move {
        tauri::async_runtime::spawn_blocking(move || {
            let managed = application.state::<ManagedStateStore>();
            let mut store = managed
                .lock_from(caller)
                .map_err(|error| format!("Could not read persisted state: {error}"))?;
            Ok(work(&mut store))
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

/// Same as [`with_store`] for fallible closures.
#[track_caller]
pub(crate) fn try_with_store<Value: Send + 'static>(
    application: &AppHandle,
    work: impl FnOnce(&mut StateStore) -> Result<Value, String> + Send + 'static,
) -> impl Future<Output = Result<Value, String>> {
    let caller = std::panic::Location::caller();
    let application = application.clone();
    async move {
        tauri::async_runtime::spawn_blocking(move || {
            let managed = application.state::<ManagedStateStore>();
            let mut store = managed
                .lock_from(caller)
                .map_err(|error| format!("Could not read persisted state: {error}"))?;
            work(&mut store)
        })
        .await
        .map_err(|error| error.to_string())?
    }
}

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
    velocity_store::save_pending(store.inner())
        .map(|_| ())
        .map_err(|error| format!("Could not save state: {error}"))
}

#[tauri::command]
#[specta::specta]
pub async fn get_settings(application: AppHandle) -> Result<AppSettings, String> {
    crate::layout::run_concurrently("get_settings", move || {
        let store = application.state::<ManagedStateStore>();
        let guard = store
            .lock()
            .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
        Ok(guard.settings().into())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_settings(
    application: AppHandle,
    settings: AppSettings,
) -> Result<AppSettings, String> {
    crate::layout::off_ui_thread_named("set_settings", move || {
        let store = application.state::<ManagedStateStore>();
        let mut guard = store
            .lock()
            .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
        let saved = guard
            .set_settings(settings.into())
            .map_err(|error| error.to_string())?;
        application
            .state::<velocity_browser::BrowserHub>()
            .limit(saved.agents.browser_limit);
        Ok(saved.into())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_ui_zoom(application: AppHandle) -> Result<UiZoomView, String> {
    let handle = application.clone();
    let view = crate::layout::run_concurrently("get_ui_zoom", move || {
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
