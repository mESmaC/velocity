//! Velocity's own updates (SPEC.md F1 "Updates"): the footer panel's commands and the
//! background check. `velocity-updates` holds the state and how this copy was installed;
//! this module runs the Tauri updater, which verifies every package against the release
//! signing key before anything is installed, and publishes `update://status`.
use std::cmp::Ordering;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tauri_specta::Event;
use velocity_core::release::{
    ManifestLag as CoreManifestLag, PublishedRelease, compare_versions, detect_manifest_lag,
    version_from_release_tag,
};
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;
use velocity_store::{AgentActivity, ThreadState};
use velocity_updates::{
    AvailableUpdate, InstallKind, ManifestLag, UpdatePhase, UpdateState, UpdateView,
};

use crate::state::ManagedStateStore;

/// The first check waits until startup work (projects, providers, the warm pool) settles.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const COLLAPSE_MANIFEST_URL: &str =
    "https://api.collapse.sh/mESmaC/velocity/releases/download/updater/latest.json";
const GITHUB_MANIFEST_URL: &str =
    "https://github.com/mESmaC/velocity/releases/download/updater/latest.json";
const GITHUB_RELEASES_API_URL: &str =
    "https://api.github.com/repos/mESmaC/velocity/releases?per_page=20";
const MANIFEST_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

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

/// Runs before the window exists: a copy a portable update just started waits for the
/// copy it replaced to exit first.
pub fn wait_after_portable_update() {
    if std::env::var_os(velocity_updates::REPLACED_MARKER).is_none() {
        return;
    }
    if let Ok(executable) = std::env::current_exe() {
        velocity_updates::wait_for_previous(&executable, Duration::from_secs(20));
    }
}

pub fn initialize(application: &AppHandle) {
    let executable = std::env::current_exe().ok();
    let kind = executable
        .as_deref()
        .map_or(InstallKind::Portable, velocity_updates::detect);
    // Portable sidecars and `taskkill`-backed helper cleanup apply only on Windows (#627).
    if cfg!(windows)
        && !matches!(kind, InstallKind::AppImage | InstallKind::Deb)
        && let Some(cleanup_executable) = executable
    {
        tauri::async_runtime::spawn_blocking(move || {
            let stopped = velocity_updates::stop_orphaned_helpers(
                &cleanup_executable,
                Duration::from_secs(5),
            );
            if !stopped.stopped.is_empty() || !stopped.still_running.is_empty() {
                crate::diagnostics::record(
                    "local build",
                    &format!(
                        "Startup orphan cleanup: stopped {:?}, still running {:?}",
                        stopped.stopped, stopped.still_running
                    ),
                );
            }
            if kind == InstallKind::Portable {
                velocity_updates::remove_previous(&cleanup_executable);
            }
        });
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
    let (kind, current_version) = {
        let mut state = lock(&updates.state);
        if !state.can_check() {
            return;
        }
        state.phase = UpdatePhase::Checking;
        (state.install_kind, state.current_version.clone())
    };
    publish(application);
    let result = find(application, kind).await;
    let found_version = result
        .as_ref()
        .ok()
        .and_then(|found| found.as_ref())
        .map(|update| update.version.clone());
    let manifest_lag = check_manifest_lag(&current_version, found_version.as_deref()).await;
    set_state(application, |state| match result {
        Ok(found) => {
            let available = found.as_ref().map(|update| AvailableUpdate {
                version: update.version.clone(),
                notes: update.body.clone().unwrap_or_default(),
            });
            *lock(&updates.found) = found;
            state.checked(available, manifest_lag, &velocity_updates::local_clock());
        }
        Err(message) => {
            state.manifest_lag = manifest_lag;
            state.failed(&message);
        }
    });
}

/// Compares Velocity's `updater/latest.json` against the newest published release so
/// installed copies report when the updater manifest is behind (#521, #593). Checks
/// Collapse first and falls back to GitHub while Collapse has no `updater` release yet.
async fn check_manifest_lag(
    current_version: &str,
    known_manifest_version: Option<&str>,
) -> Option<ManifestLag> {
    let lag = match collapse_manifest_lag(None, "mESmaC", "velocity").await {
        Some(lag) => lag,
        None => match fetch_github_manifest_lag_http(known_manifest_version).await {
            Some(lag) => Some(lag),
            None => tauri::async_runtime::spawn_blocking(|| {
                velocity_github::check_repository_manifest_lag(VELOCITY_REPOSITORY)
                    .ok()
                    .flatten()
            })
            .await
            .ok()
            .flatten(),
        },
    }?;
    let current_is_behind =
        compare_versions(current_version, &lag.latest_release) == Some(Ordering::Less);
    Some(ManifestLag {
        manifest_version: lag.manifest_version,
        latest_release: lag.latest_release,
        current_is_behind,
        latest_signed: lag.latest_signed,
    })
}

/// Checks `owner/name`'s Collapse `updater/latest.json` against Collapse's published `v*`
/// releases (#593). Returns `None` when Collapse has no `updater/latest.json` yet, or
/// `Some(lag)` when Collapse's manifest is present (`Some(None)` means up to date).
pub(crate) async fn collapse_manifest_lag(
    token: Option<&str>,
    owner: &str,
    name: &str,
) -> Option<Option<CoreManifestLag>> {
    #[derive(Deserialize)]
    struct Manifest {
        version: String,
    }
    let manifest_url = if owner.eq_ignore_ascii_case("mESmaC")
        && name.eq_ignore_ascii_case("velocity")
        && token.is_none()
    {
        COLLAPSE_MANIFEST_URL.to_owned()
    } else {
        velocity_collapse::releases::download_url(
            owner,
            name,
            velocity_collapse::releases::UPDATER_TAG,
            "latest.json",
        )
    };
    let manifest_text = velocity_collapse::releases::download_asset_text(token, &manifest_url)
        .await
        .ok()
        .flatten()?;
    let manifest = serde_json::from_str::<Manifest>(&manifest_text).ok()?;
    let page = velocity_collapse::releases::list_releases(token, owner, name, None)
        .await
        .ok()?;
    let published = collapse_to_published_releases(page.releases);
    Some(detect_manifest_lag(&manifest.version, &published))
}

fn collapse_to_published_releases(
    releases: Vec<velocity_collapse::releases::CollapseRelease>,
) -> Vec<PublishedRelease> {
    releases
        .into_iter()
        .filter_map(|release| {
            let version = version_from_release_tag(&release.tag)?;
            let has_manifest = release
                .assets
                .iter()
                .any(|asset| asset.name == "latest.json");
            let has_signature = release
                .assets
                .iter()
                .any(|asset| asset.name.ends_with(".sig"));
            Some(PublishedRelease {
                tag: release.tag,
                version,
                draft: release.draft,
                signed: has_manifest && has_signature,
            })
        })
        .collect()
}

#[derive(Deserialize)]
struct GitHubReleaseAsset {
    name: String,
}

#[derive(Deserialize)]
struct GitHubReleaseEntry {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GitHubReleaseAsset>,
}

async fn fetch_github_manifest_lag_http(
    known_manifest_version: Option<&str>,
) -> Option<CoreManifestLag> {
    #[derive(Deserialize)]
    struct Manifest {
        version: String,
    }
    let client = reqwest::Client::builder()
        .timeout(MANIFEST_FETCH_TIMEOUT)
        .user_agent("velocity-updater")
        .build()
        .ok()?;
    let manifest_version = match known_manifest_version {
        Some(version) => version.to_owned(),
        None => {
            client
                .get(GITHUB_MANIFEST_URL)
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?
                .json::<Manifest>()
                .await
                .ok()?
                .version
        }
    };
    let releases: Vec<GitHubReleaseEntry> = client
        .get(GITHUB_RELEASES_API_URL)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    let published = to_published_releases(releases);
    detect_manifest_lag(&manifest_version, &published)
}

fn to_published_releases(releases: Vec<GitHubReleaseEntry>) -> Vec<PublishedRelease> {
    releases
        .into_iter()
        .filter_map(|release| {
            let version = version_from_release_tag(&release.tag_name)?;
            let has_manifest = release
                .assets
                .iter()
                .any(|asset| asset.name == "latest.json");
            let has_signature = release
                .assets
                .iter()
                .any(|asset| asset.name.ends_with(".sig"));
            Some(PublishedRelease {
                tag: release.tag_name,
                version,
                draft: release.draft,
                signed: has_manifest && has_signature,
            })
        })
        .collect()
}

/// Each install kind checks its matching manifest entry: Windows kinds and Linux AppImage
/// use their suffixed target, while `.deb` checks the base Linux entry to surface releases.
async fn find(application: &AppHandle, kind: InstallKind) -> Result<Option<Update>, String> {
    let base = tauri_plugin_updater::target().ok_or("Updates aren't supported on this system")?;
    let updater = application
        .updater_builder()
        .target(kind.manifest_target(&base))
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
        if state.install_kind != InstallKind::Deb {
            state.downloaded(0, None);
        }
        state.install_kind
    };
    if kind == InstallKind::Deb {
        let url = deb_download_url(update.download_url.as_str(), &update.version);
        return application
            .opener()
            .open_url(&url, None::<&str>)
            .map_err(|error| format!("Could not open {url}: {error}"));
    }
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
    install_download(application, kind, &update, &bytes).await
}

fn deb_download_url(download_url: &str, version: &str) -> String {
    let file = format!("Velocity_{version}_amd64.deb");
    match download_url.rsplit_once('/') {
        Some((prefix, _)) if !prefix.is_empty() => format!("{prefix}/{file}"),
        _ => format!("https://github.com/mESmaC/velocity/releases/download/v{version}/{file}"),
    }
}

async fn install_download(
    application: &AppHandle,
    kind: InstallKind,
    update: &Update,
    bytes: &[u8],
) -> Result<(), String> {
    set_state(application, |state| state.phase = UpdatePhase::Installing);
    hand_over(application, "The update was not installed", || match kind {
        InstallKind::Portable => restart_portable(application, bytes),
        // Launches the installer and exits Velocity; it only returns on failure.
        InstallKind::Nsis | InstallKind::Msi => update
            .install(bytes)
            .map_err(|error| format!("Could not start the installer: {error}")),
        // Tauri replaces the AppImage file in place and returns; relaunch into it.
        InstallKind::AppImage => restart_appimage(application, update, bytes),
        InstallKind::Deb => Err(
            "Debian packages can't update themselves. Update with your package manager or download the new .deb package."
                .into(),
        ),
    })
    .await
}

fn restart_appimage(application: &AppHandle, update: &Update, bytes: &[u8]) -> Result<(), String> {
    update
        .install(bytes)
        .map_err(|error| format!("Could not replace the AppImage: {error}"))?;
    let appimage = std::env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .ok_or("The AppImage was replaced, but its path could not be found to restart")?;
    velocity_updates::relaunch(&appimage).map_err(|error| {
        format!("The AppImage was replaced, but Velocity could not restart: {error}")
    })?;
    application.exit(0);
    Ok(())
}

/// Starts the existing executable again if a restart fails or aborts after state was
/// saved and this window cannot keep running safely, so Velocity never stays closed (#605).
struct RelaunchOnDrop {
    executable: Option<std::path::PathBuf>,
    armed: bool,
}

impl RelaunchOnDrop {
    fn arm(executable: Option<std::path::PathBuf>) -> Self {
        Self {
            executable,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn relaunch_now(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let Some(executable) = self.executable.as_deref() else {
            return;
        };
        if let Err(error) = velocity_updates::relaunch(executable) {
            crate::diagnostics::record(
                "local build",
                &format!(
                    "Could not restart existing executable {}: {error}",
                    executable.display()
                ),
            );
        }
    }
}

impl Drop for RelaunchOnDrop {
    fn drop(&mut self) {
        self.relaunch_now();
    }
}

fn main_window_open(application: &AppHandle) -> bool {
    application
        .get_webview_window(crate::window::MAIN_WINDOW)
        .is_some()
}

/// Makes way for another copy of Velocity, then runs `replace`, which puts that copy in
/// place and exits this one. An update and a restart into a local build (#512) both go
/// through here, so both keep state and resume threads the same way. When `replace` fails,
/// work carries on in this copy, or restarts the existing executable if this copy cannot
/// safely remain open (#605).
pub(crate) async fn hand_over(
    application: &AppHandle,
    not_done: &str,
    replace: impl FnOnce() -> Result<(), String> + Send,
) -> Result<(), String> {
    // The installer exits Velocity without an exit event, so save first, and don't
    // replace this copy over changes that couldn't be saved.
    crate::state::save_now(application).map_err(|error| format!("{error}. {not_done}."))?;
    let mut fallback = RelaunchOnDrop::arm(std::env::current_exe().ok());
    crate::rosen::shutdown_servers(application).await;
    let recovery =
        application.state::<std::sync::Arc<velocity_updates::supervision::RecoveryConnection>>();
    if let Err(error) = recovery.set_intentional_exit(true) {
        crate::rosen::restart_servers(application).await;
        if main_window_open(application) {
            fallback.disarm();
        }
        return Err(format!(
            "Could not hand off recovery to the restart: {error}"
        ));
    }
    let replaced = replace();
    match replaced {
        Ok(()) => {
            fallback.disarm();
            Ok(())
        }
        Err(error) => {
            let cleared = match recovery.set_intentional_exit(false) {
                Ok(()) => true,
                Err(recovery_error) => {
                    crate::diagnostics::record("recovery", &recovery_error.to_string());
                    false
                }
            };
            crate::rosen::restart_servers(application).await;
            if cleared && main_window_open(application) {
                fallback.disarm();
            } else {
                crate::diagnostics::record(
                    "local build",
                    "Restart failed after state was saved; starting the existing executable again.",
                );
                fallback.relaunch_now();
                application.exit(0);
            }
            Err(error)
        }
    }
}

/// The caller vouches for the bytes: `download` verified an update's against the release
/// key, and a local build's were made on this machine. Any previous copy's process trees
/// and our own helper children are stopped first, then the new executable takes the
/// running one's name, starts with the same arguments, and this copy exits (#605).
pub(crate) fn restart_portable(application: &AppHandle, bytes: &[u8]) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Portable executable swap is only supported on Windows.".into());
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("Could not find Velocity's executable: {error}"))?;
    let stopped = velocity_updates::stop_before_swap(&executable, Duration::from_secs(5));
    if !stopped.stopped.is_empty() || !stopped.still_running.is_empty() {
        crate::diagnostics::record(
            "local build",
            &format!(
                "Pre-swap process cleanup: stopped {:?}, still running {:?}",
                stopped.stopped, stopped.still_running
            ),
        );
    }
    let previous =
        velocity_updates::replace_running(&executable, bytes).map_err(|error| error.to_string())?;
    if let Err(error) = velocity_updates::relaunch_replaced(&executable, &previous) {
        let restored = match velocity_updates::restore_previous(&executable, &previous) {
            Ok(()) => "the previous copy was restored".to_owned(),
            Err(restore_error) => format!(
                "could not restore the previous copy ({restore_error}); it remains at {}",
                previous.display()
            ),
        };
        return Err(format!(
            "The new version could not be started ({error}); {restored}"
        ));
    }
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

/// Agents mid-turn, which a restart interrupts: build, review or spec threads that are
/// thinking or running a tool, and busy duty agents.
pub(crate) fn working_agents(application: &AppHandle) -> usize {
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
                matches!(
                    thread.state,
                    ThreadState::Working | ThreadState::Review | ThreadState::Spec
                ) && matches!(
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
