//! Post-switch one-way sync of GitHub issues opened on the old host into Collapse (#612).
//!
//! After Move to Collapse switches a project's issue host to Collapse, issues opened on
//! GitHub by habit or by older agents still need to reach dispatch for a transition window
//! (default 14 days) or until the owner turns the sync off in Collapse settings.
//!
//! Unlike Collapse's full GitHub import (which overwrites existing Collapse issue state),
//! this sync only mirrors newly opened GitHub issues above the cut-over high-water mark
//! and deduplicates with both persisted issue mappings and a hidden body marker so active
//! Collapse issues are never clobbered or duplicated.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use tokio::sync::Notify;
use velocity_collapse::issues::{CollapseIssueApi, Conditional, CreateIssueRequest};
use velocity_core::collapse::{
    GITHUB_ISSUE_SYNC_DEFAULT_WINDOW_SECS, GitHubIssueSync, IssueHost,
    format_synced_github_issue_body, github_repository_from_remote, github_sync_expired,
    github_sync_marker, should_sync_github_issue,
};
use velocity_github::{GitHubIssue, IssueClient};

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

/// Enables (or updates) post-switch one-way GitHub issue sync after Move to Collapse or
/// Import now finishes (#612). Preserves already-synced issue mappings and refreshes the
/// default 14-day window when enabling.
pub(crate) async fn enable_post_switch_sync(
    application: &AppHandle,
    project: u32,
    github_repository: &str,
    high_water_number: Option<u32>,
    respect_explicit_disable: bool,
) -> Result<(), String> {
    let trimmed = github_repository.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let now_secs = unix_seconds();
    let github_owned = trimmed.to_owned();
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
            next.high_water_number = 0;
        }
        next.github_repository = github_owned;
        if let Some(high_water) = high_water_number {
            next.high_water_number = next.high_water_number.max(high_water);
        }
        next.enabled = true;
        next.expires_at_secs = Some(now_secs.saturating_add(GITHUB_ISSUE_SYNC_DEFAULT_WINDOW_SECS));
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

/// `set_github_issue_sync` (§3.1): turns the post-switch one-way GitHub-to-Collapse issue
/// sync on or off for a Collapse-hosted project (#612).
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
    let high_water = seed_high_water(
        &subject.repository_path,
        &collapse_repository,
        &github_repository,
    )
    .await;
    enable_post_switch_sync(
        &application,
        project_identifier,
        &github_repository,
        high_water,
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

/// `sync_github_issues_now` (§3.1): runs one immediate pass of the post-switch one-way
/// GitHub-to-Collapse issue sync for `project_identifier` (#612).
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
        "GitHub issue sync is up to date (no new GitHub issues).".to_owned()
    } else if synced == 1 {
        "Synced 1 new GitHub issue to Collapse.".to_owned()
    } else {
        format!("Synced {synced} new GitHub issues to Collapse.")
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
    let now_secs = now_ms / 1000;
    if github_sync_expired(&candidate.sync, now_secs) {
        candidate.sync.enabled = false;
        candidate.sync.last_error = None;
        save_sync_state(application, candidate.project, candidate.sync.clone()).await?;
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "{}: one-way GitHub issue sync window expired and turned off",
                candidate.name
            ),
        );
        return Ok(0);
    }
    let Some(write_token) =
        tauri::async_runtime::spawn_blocking(velocity_collapse::load_issues_token)
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?
    else {
        let message = "Save a Collapse PAT with issues scope to sync new GitHub issues.".to_owned();
        record_sync_error(application, candidate, message.clone()).await;
        return Err(message);
    };
    let Some((owner, name)) = candidate.collapse_repository.split_once('/') else {
        return Ok(0);
    };
    let (owner, name) = (owner.to_owned(), name.to_owned());
    let github_issues = read_github_open_issues(
        &candidate.repository_path,
        &candidate.sync.github_repository,
    )
    .await;
    let github_issues = match github_issues {
        Ok(issues) => issues,
        Err(error) => {
            record_sync_error(application, candidate, error.clone()).await;
            return Err(format!("{}: {error}", candidate.name));
        }
    };
    let mut pending: Vec<GitHubIssue> = github_issues
        .into_iter()
        .filter(|issue| should_sync_github_issue(&candidate.sync, issue.number, now_secs))
        .collect();
    pending.sort_by_key(|issue| issue.number);
    let created = if pending.is_empty() {
        0
    } else {
        match mirror_pending_issues(candidate, &write_token, &owner, &name, &pending).await {
            Ok(count) => count,
            Err(error) => {
                record_sync_error(application, candidate, error.clone()).await;
                return Err(format!("{}: {error}", candidate.name));
            }
        }
    };
    candidate.sync.last_synced_at_ms = Some(now_ms);
    candidate.sync.last_error = None;
    save_sync_state(application, candidate.project, candidate.sync.clone()).await?;
    if created > 0 {
        IssueClient::invalidate_open_issues_for(
            IssueHost::Collapse,
            &candidate.collapse_repository,
        );
    }
    Ok(created)
}

async fn mirror_pending_issues(
    candidate: &mut SyncCandidate,
    write_token: &str,
    owner: &str,
    name: &str,
    pending: &[GitHubIssue],
) -> Result<usize, String> {
    let existing_collapse =
        read_collapse_open_issues(&candidate.repository_path, &candidate.collapse_repository)
            .await
            .unwrap_or_default();
    let api = CollapseIssueApi::new().map_err(|error| error.to_string())?;
    let available_labels = fetch_collapse_labels(&api, write_token, owner, name).await;
    let mut created_count = 0_usize;
    for issue in pending {
        let marker = github_sync_marker(&candidate.sync.github_repository, issue.number);
        if let Some(existing) = existing_collapse
            .iter()
            .find(|row| row.body.contains(&marker))
        {
            candidate
                .sync
                .synced_numbers
                .insert(issue.number, existing.number);
            candidate.sync.high_water_number = candidate.sync.high_water_number.max(issue.number);
            continue;
        }
        let body = format_synced_github_issue_body(
            &issue.body,
            &candidate.sync.github_repository,
            issue.number,
        );
        let labels = match_existing_labels(&issue.labels, &available_labels);
        let blocked_by = map_blockers(
            &issue.open_blockers,
            &candidate.sync.synced_numbers,
            candidate.sync.high_water_number,
        );
        let created = api
            .create_issue(
                write_token,
                owner,
                name,
                &CreateIssueRequest {
                    title: &issue.title,
                    body: &body,
                    labels,
                    blocked_by,
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        candidate
            .sync
            .synced_numbers
            .insert(issue.number, created.number);
        candidate.sync.high_water_number = candidate.sync.high_water_number.max(issue.number);
        created_count += 1;
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "{}: synced GitHub {}#{} -> Collapse {}#{}",
                candidate.name,
                candidate.sync.github_repository,
                issue.number,
                candidate.collapse_repository,
                created.number
            ),
        );
    }
    Ok(created_count)
}

async fn fetch_collapse_labels(
    api: &CollapseIssueApi,
    token: &str,
    owner: &str,
    name: &str,
) -> Vec<String> {
    match api.list_labels(Some(token), owner, name, None).await {
        Ok(Conditional::Modified { value, .. }) => {
            value.labels.into_iter().map(|label| label.name).collect()
        }
        _ => Vec::new(),
    }
}

fn match_existing_labels(requested: &[String], available: &[String]) -> Vec<String> {
    requested
        .iter()
        .filter_map(|label| {
            available
                .iter()
                .find(|existing| existing.eq_ignore_ascii_case(label))
                .cloned()
        })
        .collect()
}

fn map_blockers(
    open_blockers: &[String],
    synced_numbers: &BTreeMap<u32, u32>,
    high_water_number: u32,
) -> Vec<String> {
    let mut mapped = Vec::new();
    for blocker in open_blockers {
        if let Some(raw_number) = blocker.strip_prefix('#')
            && let Ok(number) = raw_number.parse::<u32>()
        {
            if let Some(&collapse_number) = synced_numbers.get(&number) {
                mapped.push(format!("#{collapse_number}"));
            } else if number <= high_water_number {
                mapped.push(format!("#{number}"));
            }
        }
    }
    mapped
}

async fn read_github_open_issues(
    repository_path: &Path,
    github_repository: &str,
) -> Result<Vec<GitHubIssue>, String> {
    let directory = repository_path.to_path_buf();
    let repo = github_repository.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let client =
            IssueClient::for_repository(&directory, &repo).map_err(|error| error.to_string())?;
        client.open_issues().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn read_collapse_open_issues(
    repository_path: &Path,
    collapse_repository: &str,
) -> Result<Vec<GitHubIssue>, String> {
    let directory = repository_path.to_path_buf();
    let repo = collapse_repository.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let client =
            IssueClient::for_collapse(&directory, &repo).map_err(|error| error.to_string())?;
        client.open_issues().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn resolve_sync_github_repository(
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

async fn seed_high_water(
    repository_path: &Path,
    collapse_repository: &str,
    github_repository: &str,
) -> Option<u32> {
    if let Some((owner, name)) = collapse_repository.split_once('/')
        && let Ok(Ok(Some(token))) =
            tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token).await
        && let Ok(status) = velocity_collapse::get_issue_import_status(&token, owner, name).await
        && let Some(result) = status.result
        && result.next_number > 1
    {
        return u32::try_from(result.next_number.saturating_sub(1)).ok();
    }
    read_github_open_issues(repository_path, github_repository)
        .await
        .ok()
        .map(|issues| issues.iter().map(|issue| issue.number).max().unwrap_or(0))
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

fn unix_seconds() -> u64 {
    unix_millis() / 1000
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}
