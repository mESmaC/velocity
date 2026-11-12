//! Velocity's own updates (SPEC.md F1 "Updates"): the footer panel's commands and the
//! background check. `velocity-updates` holds the state and how this copy was installed;
//! this module runs the Tauri updater, which verifies every package against the release
//! signing key before anything is installed, and publishes `update://status`.
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};
use tauri_specta::Event;
use velocity_store::{AgentActivity, ThreadState};
use velocity_updates::{AvailableUpdate, InstallKind, UpdatePhase, UpdateState, UpdateView};

use crate::state::ManagedStateStore;

/// The first check waits until startup work (projects, providers, the warm pool) settles.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub struct ManagedUpdates {
    state: Mutex<UpdateState>,
    /// The release the last check found, kept so Install takes exactly what was shown.
    found: Mutex<Option<Update>>,
}

#[derive(Clone, serde::Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "update://status")]
pub struct UpdateStatusChanged {
    pub view: UpdateView,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn initialize(application: &AppHandle) {
    let executable = std::env::current_exe().ok();
    let kind = executable
        .as_deref()
        .map_or(InstallKind::Portable, velocity_updates::detect);
    if kind == InstallKind::Portable
        && let Some(executable) = &executable
    {
        velocity_updates::remove_previous(executable);
    }
    let version = application.package_info().version.to_string();
    application.manage(ManagedUpdates {
        state: Mutex::new(UpdateState::new(&version, kind)),
        found: Mutex::new(None),
    });
    // Development builds check only when asked, so `cargo tauri dev` never nags.
    if cfg!(debug_assertions) {
        return;
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            check(&handle).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

#[tauri::command]
#[specta::specta]
pub fn get_update_status(application: AppHandle) -> UpdateView {
    view(&application)
}

#[tauri::command]
#[specta::specta]
pub async fn check_for_update(application: AppHandle) -> UpdateView {
    check(&application).await;
    view(&application)
}

/// Downloads the release the last check found, then installs it and restarts. Failures
/// land in the panel's status, never as an error the window keeps showing.
#[tauri::command]
#[specta::specta]
pub async fn install_update(application: AppHandle) -> UpdateView {
    if let Err(message) = install(&application).await {
        set_state(&application, |state| state.failed(&message));
    }
    view(&application)
}

async fn check(application: &AppHandle) {
    let Some(updates) = application.try_state::<ManagedUpdates>() else {
        return;
    };
    let kind = {
        let mut state = lock(&updates.state);
        if !state.can_check() {
            return;
        }
        state.phase = UpdatePhase::Checking;
        state.install_kind
    };
    publish(application);
    let result = find(application, kind).await;
    set_state(application, |state| match result {
        Ok(found) => {
            let available = found.as_ref().map(|update| AvailableUpdate {
                version: update.version.clone(),
                notes: update.body.clone().unwrap_or_default(),
            });
            *lock(&updates.found) = found;
            state.checked(available, &velocity_updates::local_clock());
        }
        Err(message) => state.failed(&message),
    });
}

/// Each install kind has its own entry in the manifest, so a portable copy never runs
/// the setup and an installed copy never swaps its executable.
async fn find(application: &AppHandle, kind: InstallKind) -> Result<Option<Update>, String> {
    let base = tauri_plugin_updater::target().ok_or("Updates aren't supported on this system")?;
    let updater = application
        .updater_builder()
        .target(format!("{base}-{}", kind.manifest_suffix()))
        .build()
        .map_err(|error| error.to_string())?;
    match updater.check().await {
        Ok(found) => Ok(found),
        // No manifest published yet: nothing newer exists.
        Err(tauri_plugin_updater::Error::ReleaseNotFound) => Ok(None),
        Err(error) => Err(format!("Could not check for updates: {error}")),
    }
}

async fn install(application: &AppHandle) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err("Development builds don't install updates. Use a release build.".into());
    }
    let updates = application
        .try_state::<ManagedUpdates>()
        .ok_or("Updates haven't started yet")?;
    let update = lock(&updates.found)
        .clone()
        .ok_or("No update is available. Check for updates first.")?;
    let kind = {
        let mut state = lock(&updates.state);
        if state.phase != UpdatePhase::Available {
            return Err("No update is ready to install. Check for updates first.".into());
        }
        state.downloaded(0, None);
        state.install_kind
    };
    publish(application);
    let progress = application.clone();
    let bytes = update
        .download(
            move |chunk, total| {
                let Some(updates) = progress.try_state::<ManagedUpdates>() else {
                    return;
                };
                let changed = lock(&updates.state).downloaded(chunk as u64, total);
                if changed {
                    publish(&progress);
                }
            },
            || {},
        )
        .await
        .map_err(|error| format!("Could not download the update: {error}"))?;
    set_state(application, |state| state.phase = UpdatePhase::Installing);
    match kind {
        InstallKind::Portable => restart_portable(application, &bytes),
        // Launches the installer and exits Velocity; it only returns on failure.
        InstallKind::Nsis | InstallKind::Msi => update
            .install(&bytes)
            .map_err(|error| format!("Could not start the installer: {error}")),
    }
}

/// The bytes were verified by `download`. The new executable takes the running one's name,
/// starts with the same arguments, and this copy exits.
fn restart_portable(application: &AppHandle, bytes: &[u8]) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("Could not find Velocity's executable: {error}"))?;
    velocity_updates::replace_running(&executable, bytes).map_err(|error| error.to_string())?;
    std::process::Command::new(&executable)
        .args(std::env::args_os().skip(1))
        .spawn()
        .map_err(|error| {
            format!("The new version is in place but didn't start ({error}). Start Velocity again.")
        })?;
    application.exit(0);
    Ok(())
}

fn set_state(application: &AppHandle, change: impl FnOnce(&mut UpdateState)) {
    if let Some(updates) = application.try_state::<ManagedUpdates>() {
        change(&mut lock(&updates.state));
    }
    publish(application);
}

fn view(application: &AppHandle) -> UpdateView {
    let working = working_agents(application);
    match application.try_state::<ManagedUpdates>() {
        Some(updates) => lock(&updates.state).view(working),
        None => UpdateState::new(
            &application.package_info().version.to_string(),
            InstallKind::Portable,
        )
        .view(working),
    }
}

/// Agents mid-turn, which a restart interrupts: build threads that are thinking or running
/// a tool, and busy duty agents.
fn working_agents(application: &AppHandle) -> usize {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return 0;
    };
    let Ok(store) = store.lock() else {
        return 0;
    };
    store
        .state()
        .projects
        .iter()
        .map(|project| {
            let threads = project.threads.iter().filter(|thread| {
                matches!(thread.state, ThreadState::Working | ThreadState::Review)
                    && matches!(
                        thread.activity,
                        AgentActivity::Thinking | AgentActivity::Executing | AgentActivity::Stalled
                    )
            });
            let duty = project
                .duty_roles
                .iter()
                .flat_map(|role| role.agents.iter())
                .filter(|agent| agent.busy);
            threads.count() + duty.count()
        })
        .sum()
}

fn publish(application: &AppHandle) {
    let view = view(application);
    if let Err(error) = (UpdateStatusChanged { view }).emit(application) {
        eprintln!("Could not publish the update status: {error}");
    }
}
