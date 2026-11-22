//! Post-import one-way sync of GitHub issues into Collapse (#612, #614).
//!
//! Covers every GitHub issue opened or changed since Collapse's import `finished_at`
//! (not merely since the host switch) until the owner turns the sync off in Collapse
//! settings. Uses Collapse's lossless repository import when Collapse's issue cursor has
//! not advanced past the last import so GitHub issues keep their exact numbers and
//! Collapse's `next_number` advances past them; otherwise mirrors new and changed issues
//! through the Collapse issue API without clobbering Collapse edits.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use tokio::sync::Notify;
use velocity_core::collapse::{GitHubIssueSync, IssueHost, github_repository_from_remote};
use velocity_github::github_issue_sync::sync_github_issues_async;

use crate::collapse_mirror::{
    ProjectCollapseSettings, mirror_subject, read_project_collapse_settings, toast,
};

const SOURCE: &str = "collapse issue sync";
const FIRST_SYNC_DELAY: Duration = Duration::from_secs(45);
const SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);

struct SyncCoordinator {
    wake: Notify,
    in_flight: Mutex<HashSet<u32>>,
}

impl Default for SyncCoordinator {
    fn default() -> Self {
        Self {
            wake: Notify::new(),
            in_flight: Mutex::new(HashSet::new()),
        }
    }
}

struct SyncCandidate {
    project: u32,
    name: String,
    repository_path: PathBuf,
    collapse_repository: String,
    sync: GitHubIssueSync,
}

pub fn initialize(application: &AppHandle) {
    application.manage(SyncCoordinator::default());
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_SYNC_DELAY).await;
        loop {
            sync_all_projects(&handle).await;
            let coordinator = handle.try_state::<SyncCoordinator>();
            match coordinator {
                Some(state) => {
                    let _ = tokio::time::timeout(SYNC_INTERVAL, state.wake.notified()).await;
                }
                None => tokio::time::sleep(SYNC_INTERVAL).await,
            }
        }
    });
}

pub(crate) fn wake(application: &AppHandle) {
    if let Some(coordinator) = application.try_state::<SyncCoordinator>() {
        coordinator.wake.notify_one();
    }
}

/// Enables (or updates) one-way GitHub issue sync after Move to Collapse, Import now, or
/// switching a project's issue host to Collapse (#612, #614). Sync stays on until the
/// owner turns it off and covers issues opened or changed since `import_finished_at`.
pub(crate) async fn enable_post_switch_sync(
    application: &AppHandle,
    project: u32,
    github_repository: &str,
    high_water_number: Option<u32>,
    import_finished_at: Option<&str>,
    respect_explicit_disable: bool,
) -> Result<(), String> {
    let trimmed = github_repository.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let github_owned = trimmed.to_owned();
    let finished_owned = import_finished_at
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let changed = crate::state::try_with_store(application, move |store| {
        let Some(item) = store.project(project) else {
            return Ok(false);
        };
        if respect_explicit_disable
            && item
                .github_issue_sync
                .as_ref()
                .is_some_and(|existing| !existing.enabled && !existing.github_repository.is_empty())
        {
            return Ok(false);
        }
        let mut next = item.github_issue_sync.clone().unwrap_or_default();
        if !next.github_repository.eq_ignore_ascii_case(&github_owned) {
            next.synced_numbers.clear();
            next.synced_updated_at.clear();
            next.high_water_number = 0;
            next.import_finished_at = None;
        }
        next.github_repository = github_owned;
        if let Some(high_water) = high_water_number {
            next.high_water_number = next.high_water_number.max(high_water);
        }
        if let Some(finished_at) = finished_owned {
            next.import_finished_at = Some(finished_at);
        }
        next.enabled = true;
        next.expires_at_secs = None;
        next.last_error = None;
        store
            .save_github_issue_sync(project, Some(next))
            .map_err(|error| error.to_string())
    })
    .await?;
    if changed {
        wake(application);
    }
    Ok(())
}

/// Seeds and enables one-way GitHub issue sync from Collapse's recorded import status
/// when a project's issue host switches to Collapse (#614).
pub(crate) async fn enable_sync_from_import_status(
    application: &AppHandle,
    project: u32,
    repository_path: &Path,
    collapse_repository: &str,
    respect_explicit_disable: bool,
) -> Result<(), String> {
    let Some(github_repository) =
        resolve_sync_github_repository(application, project, repository_path, collapse_repository)
            .await?
    else {
        return Ok(());
    };
    let (import_finished_at, high_water) = seed_from_import(collapse_repository).await;
    enable_post_switch_sync(
        application,
        project,
        &github_repository,
        high_water,
        import_finished_at.as_deref(),
        respect_explicit_disable,
    )
    .await
}

/// `set_github_issue_sync` (§3.1): turns the one-way GitHub-to-Collapse issue sync on or
/// off for a Collapse-hosted project (#612, #614).
#[tauri::command]
#[specta::specta]
pub async fn set_github_issue_sync(
    application: AppHandle,
    project_identifier: u32,
    enabled: bool,
) -> Result<ProjectCollapseSettings, String> {
    let subject = mirror_subject(&application, project_identifier)?;
    if !enabled {
        disable_sync(&application, project_identifier).await?;
        let _ = toast(
            &application,
            format!("{}: turned off one-way GitHub issue sync", subject.name),
        );
        return read_project_collapse_settings(&application, project_identifier).await;
    }
    let collapse_repository = subject
        .repository
        .clone()
        .ok_or("Mirror this project to Collapse before enabling GitHub issue sync")?;
    let github_repository = resolve_sync_github_repository(
        &application,
        project_identifier,
        &subject.repository_path,
        &collapse_repository,
    )
    .await?
    .ok_or("No GitHub repository found for this project; set one in Move to Collapse first")?;
    let (import_finished_at, high_water) = seed_from_import(&collapse_repository).await;
    enable_post_switch_sync(
        &application,
        project_identifier,
        &github_repository,
        high_water,
        import_finished_at.as_deref(),
        false,
    )
    .await?;
    let _ = toast(
        &application,
        format!(
            "{}: enabled one-way GitHub issue sync from {github_repository}",
            subject.name
        ),
    );
    read_project_collapse_settings(&application, project_identifier).await
}

/// `sync_github_issues_now` (§3.1): runs one immediate pass of the one-way
/// GitHub-to-Collapse issue sync for `project_identifier` (#612, #614).
#[tauri::command]
#[specta::specta]
pub async fn sync_github_issues_now(
    application: AppHandle,
    project_identifier: u32,
) -> Result<ProjectCollapseSettings, String> {
    let candidate = load_candidate(&application, project_identifier)
        .await?
        .ok_or("Enable one-way GitHub issue sync on a Collapse-hosted project first")?;
    let synced = sync_one_project(&application, candidate).await?;
    let message = if synced == 0 {
        "GitHub issue sync is up to date (no new or changed GitHub issues).".to_owned()
    } else if synced == 1 {
        "Synced 1 GitHub issue to Collapse.".to_owned()
    } else {
        format!("Synced {synced} GitHub issues to Collapse.")
    };
    let _ = toast(&application, message);
    read_project_collapse_settings(&application, project_identifier).await
}

async fn disable_sync(application: &AppHandle, project: u32) -> Result<(), String> {
    crate::state::try_with_store(application, move |store| {
        let Some(mut sync) = store
            .project(project)
            .and_then(|item| item.github_issue_sync.clone())
        else {
            return Ok(());
        };
        sync.enabled = false;
        sync.expires_at_secs = None;
        sync.last_error = None;
        store
            .save_github_issue_sync(project, Some(sync))
            .map_err(|error| error.to_string())?;
        Ok(())
    })
    .await
}

async fn sync_all_projects(application: &AppHandle) {
    let candidates = active_candidates(application).await;
    for candidate in candidates {
        if let Err(error) = sync_one_project(application, candidate).await {
            crate::diagnostics::record(SOURCE, &error);
        }
    }
}

async fn active_candidates(application: &AppHandle) -> Vec<SyncCandidate> {
    crate::state::with_store(application, |store| {
        store
            .state()
            .projects
            .iter()
            .filter_map(|item| {
                if item.effective_issue_host() != IssueHost::Collapse {
                    return None;
                }
                let collapse_repository = item.collapse_repository.clone()?;
                let sync = item.github_issue_sync.clone()?;
                if !sync.enabled || sync.github_repository.trim().is_empty() {
                    return None;
                }
                Some(SyncCandidate {
                    project: item.id,
                    name: item.name.clone(),
                    repository_path: PathBuf::from(&item.repository_path),
                    collapse_repository,
                    sync,
                })
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

async fn load_candidate(
    application: &AppHandle,
    project: u32,
) -> Result<Option<SyncCandidate>, String> {
    crate::state::try_with_store(application, move |store| {
        let Some(item) = store.project(project) else {
            return Err("Unknown project".into());
        };
        if item.effective_issue_host() != IssueHost::Collapse {
            return Ok(None);
        }
        let Some(collapse_repository) = item.collapse_repository.clone() else {
            return Ok(None);
        };
        let Some(sync) = item.github_issue_sync.clone() else {
            return Ok(None);
        };
        if !sync.enabled || sync.github_repository.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(SyncCandidate {
            project: item.id,
            name: item.name.clone(),
            repository_path: PathBuf::from(&item.repository_path),
            collapse_repository,
            sync,
        }))
    })
    .await
}

fn acquire_in_flight(application: &AppHandle, project: u32) -> bool {
    let Some(coordinator) = application.try_state::<SyncCoordinator>() else {
        return true;
    };
    let mut active = coordinator
        .in_flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    active.insert(project)
}

fn release_in_flight(application: &AppHandle, project: u32) {
    if let Some(coordinator) = application.try_state::<SyncCoordinator>() {
        let mut active = coordinator
            .in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.remove(&project);
    }
}

async fn sync_one_project(
    application: &AppHandle,
    mut candidate: SyncCandidate,
) -> Result<usize, String> {
    if !acquire_in_flight(application, candidate.project) {
        return Ok(0);
    }
    let project_id = candidate.project;
    let outcome = run_project_sync(application, &mut candidate).await;
    release_in_flight(application, project_id);
    outcome
}

async fn run_project_sync(
    application: &AppHandle,
    candidate: &mut SyncCandidate,
) -> Result<usize, String> {
    let now_ms = unix_millis();
    candidate.sync.expires_at_secs = None;
    let Some(write_token) =
        tauri::async_runtime::spawn_blocking(velocity_collapse::load_issues_token)
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
    else {
        let message = "Save a Collapse PAT with issues scope to sync GitHub issues.".to_owned();
        record_sync_error(application, candidate, message.clone()).await;
        return Err(message);
    };
    let outcome = match sync_github_issues_async(
        &candidate.repository_path,
        &candidate.collapse_repository,
        &write_token,
        &mut candidate.sync,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            record_sync_error(application, candidate, error.clone()).await;
            return Err(format!("{}: {error}", candidate.name));
        }
    };
    for note in &outcome.notes {
        crate::diagnostics::record(SOURCE, &format!("{}: {note}", candidate.name));
    }
    candidate.sync.last_synced_at_ms = Some(now_ms);
    candidate.sync.last_error = None;
    save_sync_state(application, candidate.project, candidate.sync.clone()).await?;
    Ok(outcome.total_synced())
}

pub(crate) async fn resolve_sync_github_repository(
    application: &AppHandle,
    project: u32,
    repository_path: &Path,
    collapse_repository: &str,
) -> Result<Option<String>, String> {
    let from_store = crate::state::with_store(application, move |store| {
        let item = store.project(project)?;
        item.github_issue_sync
            .as_ref()
            .map(|sync| sync.github_repository.trim().to_owned())
            .filter(|repo| !repo.is_empty())
            .or_else(|| {
                item.github_remote.as_deref().map(|saved| {
                    github_repository_from_remote(saved).unwrap_or_else(|| saved.to_owned())
                })
            })
            .or_else(|| {
                item.collapse_mirror_health
                    .as_ref()
                    .and_then(|health| health.downstream_github.clone())
            })
    })
    .await
    .ok()
    .flatten();
    if from_store.is_some() {
        return Ok(from_store);
    }
    if let Some((owner, name)) = collapse_repository.split_once('/')
        && let Ok(Some(token)) =
            tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token)
                .await
                .map_err(|error| error.to_string())?
        && let Ok(status) = velocity_collapse::get_issue_import_status(&token, owner, name).await
    {
        for candidate in [status.github_repository, status.suggested_github_repository] {
            let trimmed = candidate.trim();
            if !trimmed.is_empty() {
                return Ok(Some(trimmed.to_owned()));
            }
        }
    }
    let path = repository_path.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        velocity_github::checkout_repository(&path).map(|name| name.full_name())
    })
    .await
    .map_err(|error| error.to_string())
}

async fn seed_from_import(collapse_repository: &str) -> (Option<String>, Option<u32>) {
    let Some((owner, name)) = collapse_repository.split_once('/') else {
        return (None, None);
    };
    let Ok(Ok(Some(token))) =
        tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token).await
    else {
        return (None, None);
    };
    let Ok(status) = velocity_collapse::get_issue_import_status(&token, owner, name).await else {
        return (None, None);
    };
    let finished_at = status.finished_without_error_at().map(str::to_owned);
    let high_water = status
        .result
        .filter(|result| result.next_number > 1)
        .and_then(|result| u32::try_from(result.next_number.saturating_sub(1)).ok());
    (finished_at, high_water)
}

async fn record_sync_error(
    application: &AppHandle,
    candidate: &mut SyncCandidate,
    message: String,
) {
    candidate.sync.last_error = Some(message);
    let _ = save_sync_state(application, candidate.project, candidate.sync.clone()).await;
}

async fn save_sync_state(
    application: &AppHandle,
    project: u32,
    sync: GitHubIssueSync,
) -> Result<(), String> {
    crate::state::try_with_store(application, move |store| {
        store
            .save_github_issue_sync(project, Some(sync))
            .map_err(|error| error.to_string())?;
        Ok(())
    })
    .await
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}
