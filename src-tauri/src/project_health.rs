//! Per-project health checks with safe automatic remediation (#509).
//!
//! Replaces external watcher scripts by observing each Rosen project's mirror, integration
//! builds, GitHub base CI, stalled landed issues (#504), parked no-change issues (#506),
//! threads stuck in review (#487), idle continuous-dispatch slots, and post-land checks
//! stopped for running too long (#486).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::base_ci::{
    BaseCiSource, BaseCiState, CommitOrder, OpenRepair, RepairThreadState,
    format_ci_repair_action_for_source,
};
use velocity_core::collapse::{
    COLLAPSE_ISSUES_NOT_IMPORTED_SIGNAL, ForwarderState, IssueHost,
    collapse_forwarding_needs_attention, collapse_forwarding_signal,
    collapse_import_never_finished, collapse_issues_import_warning_needed,
    downstream_settings_page,
};
use velocity_core::project_health::{
    AIRC_ISSUE_FAILURE_TTL_SECONDS, DISPATCH_IDLE_KEY, DispatchIdleIssueAction, DispatchIdleSample,
    DispatchIdleTracker, HOST_SOURCE_CHANGED_KEY, HealthSeverity, HealthSignalKind,
    INTEGRATION_UNCOVERED_LANDS, INTEGRATION_UNCOVERED_SECONDS, MIRROR_BEHIND_LIMIT_SECONDS,
    POST_LAND_STALL_LIMIT_SECONDS, ProjectHealthHistoryEntry, ProjectHealthItem, ProjectHealthView,
    REVIEW_STUCK_LIMIT_SECONDS, ReopenedLandedNotice, airc_issue_tool_failure_item,
    dispatch_idle_item, dispatch_idle_notification_text, format_duration,
    format_stopped_check_detail, host_issue_problem_item, host_source_change_item,
    reopened_already_landed_item, short_commit,
};
use velocity_core::release::{
    ManifestLag, RefusedGateRecovery, is_beta_release_tag, plan_refused_gate_recovery,
};
use velocity_core::rosen_checker::StoppedCheck;
use velocity_github::{IssueClient, RefusedReleaseGate};
use velocity_rosen::landing::LandCheckOutcome;
use velocity_rosen::{ProjectServer, integration, post_land};
use velocity_store::continuous_dispatch::busy_count;
use velocity_store::{ProjectItem, ReviewStage, ThreadState};
use velocity_threads::continuous_dispatch::{
    issue_number_from_piece, partition_skipped_issues, stale_claim_reason, summarize_skipped_issues,
};
use velocity_threads::landing_runner::refusal_instructions;

use crate::continuous_dispatch::DispatchHealthSnapshot;
use crate::rosen_mirror::MirrorRetryOutcome;

const SOURCE: &str = "project health";
const COLLAPSE_FORWARDING_KEY: &str = "collapse-forwarding";
const COLLAPSE_ISSUES_NOT_IMPORTED_KEY: &str = "collapse-issues-not-imported";
const LOCAL_BUILD_RESTART_FAILED_KEY: &str = "local-build-restart-failed";
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(60);
const MIRROR_RETRY_COOLDOWN_SECS: u64 = 10 * 60;
const REVIEW_CHECK_COOLDOWN_SECS: u64 = 10 * 60;
const COLLAPSE_IMPORT_CHECK_COOLDOWN_SECS: u64 = 5 * 60;
const RECENT_ACTION_TTL_SECS: u64 = 20 * 60;
const MANUAL_PARKED_LIMIT_SECS: u64 = 2 * 60 * 60;
const RELEASE_HEALTH_COOLDOWN_SECS: u64 = 15 * 60;
const RELEASE_GATE_POLL_SECS: u64 = 2 * 60;

#[derive(Clone, serde::Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "project://health")]
pub struct ProjectHealthChanged {
    pub view: ProjectHealthView,
}

#[derive(Default)]
pub struct ManagedProjectHealth {
    state: Mutex<HealthRegistry>,
}

#[derive(Default)]
struct HealthRegistry {
    projects: BTreeMap<u32, ProjectHealthTracker>,
    views: BTreeMap<u32, Vec<ProjectHealthItem>>,
}

#[derive(Default)]
struct ProjectHealthTracker {
    mirror_attempt: Option<MirrorAttempt>,
    review_seen_secs: HashMap<(u32, u64), u64>,
    review_checked: HashMap<(u32, u64), ReviewAttempt>,
    recent_actions: BTreeMap<String, RecentAction>,
    dispatch_idle: DispatchIdleTracker,
    parked: BTreeMap<u32, ParkedRecord>,
    stopped_check: Option<StoppedCheckRecord>,
    acted_times: HashMap<String, (String, String, u64)>,
    release_health: Option<ReleaseHealthCache>,
    pending_release: Option<PendingRelease>,
    retried_release_runs: HashSet<u64>,
    collapse_forwarding: Option<CollapseForwardingRecord>,
    collapse_import: Option<CollapseImportCache>,
}

#[derive(Clone)]
struct CollapseImportCache {
    collapse_repository: String,
    checked_at_secs: u64,
    import_finished: bool,
    running: bool,
    github_repository: Option<String>,
    collapse_issue_count: Option<u64>,
    last_error: Option<String>,
}

#[derive(Clone)]
struct CollapseForwardingRecord {
    collapse_repository: String,
    github_repository: String,
    reason: String,
    opened_setup_page: bool,
    acted_at_ms: u64,
}

#[derive(Clone)]
struct ReleaseHealthCache {
    checked_at_secs: u64,
    manifest_lag: Option<ManifestLag>,
    refused_gate: Option<RefusedReleaseGate>,
}

#[derive(Clone)]
struct PendingRelease {
    tag: String,
    commit: String,
}

#[derive(Clone)]
struct MirrorAttempt {
    base_commit: String,
    pushed_commit: Option<String>,
    at_secs: u64,
    action: String,
    severity: HealthSeverity,
}

#[derive(Clone)]
struct ReviewAttempt {
    at_secs: u64,
    action: String,
    severity: HealthSeverity,
}

#[derive(Clone)]
struct RecentAction {
    item: ProjectHealthItem,
    expires_secs: u64,
}

#[derive(Clone)]
enum ParkedRecord {
    Manual {
        seen_secs: u64,
    },
    Failed {
        error: String,
        acted_at_ms: u64,
    },
    Resolved {
        action: String,
        acted_at_ms: u64,
        expires_secs: u64,
    },
}

#[derive(Clone)]
struct StoppedCheckRecord {
    commit: String,
    command: String,
    stopped: StoppedCheck,
    acted_at_ms: u64,
}

fn registry(managed: &ManagedProjectHealth) -> MutexGuard<'_, HealthRegistry> {
    managed
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn initialize(application: &AppHandle) {
    application.manage(ManagedProjectHealth::default());
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK).await;
        loop {
            check_all_projects(&handle).await;
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// `get_project_health` (§3.1): returns the active health signals for one project (#509).
#[tauri::command]
#[specta::specta]
pub fn get_project_health(application: AppHandle, project_identifier: u32) -> ProjectHealthView {
    let mut items = application
        .try_state::<ManagedProjectHealth>()
        .map(|managed| {
            registry(&managed)
                .views
                .get(&project_identifier)
                .cloned()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let mut history = Vec::new();
    if let Some(project) = persisted_project(&application, project_identifier) {
        history = project.health_history.clone();
        if !items.iter().any(|item| item.key == COLLAPSE_FORWARDING_KEY)
            && let Some(item) = check_collapse_forwarding(&application, &project, unix_millis())
        {
            upsert_item(&mut items, item);
        }
        if !items.iter().any(|item| item.key == HOST_SOURCE_CHANGED_KEY)
            && let Some(item) = check_host_source_change(&project)
        {
            upsert_item(&mut items, item);
        }
        for item in check_host_issue_problems(&project) {
            if !items.iter().any(|existing| existing.key == item.key) {
                upsert_item(&mut items, item);
            }
        }
        for item in check_reopened_landed_holds(&project) {
            if !items.iter().any(|existing| existing.key == item.key) {
                upsert_item(&mut items, item);
            }
        }
    }
    ProjectHealthView {
        project_identifier,
        items,
        history,
    }
}

/// `act_on_dispatch_idle_issue` (§3.1): runs a one-click owner unblock action on a blocked
/// issue from the dispatch-idle alert (`retry_attempt`, `mark_needs_owner`, or `lift_hold`),
/// refreshes the dispatch health view, and returns the updated view (#616).
#[tauri::command]
#[specta::specta]
pub async fn act_on_dispatch_idle_issue(
    application: AppHandle,
    project_identifier: u32,
    repository: String,
    issue_number: u32,
    action: DispatchIdleIssueAction,
) -> Result<ProjectHealthView, String> {
    let project =
        persisted_project(&application, project_identifier).ok_or("The project was removed")?;
    let repo = repository.trim().to_owned();
    if repo.is_empty() {
        return Err("Missing issue repository".into());
    }
    let toast_message =
        run_dispatch_idle_issue_action(&application, &project, repo, issue_number, action).await?;
    refresh_dispatch_idle_view(&application, project_identifier);
    crate::diagnostics::record(SOURCE, &toast_message);
    let _ = crate::collapse_mirror::toast(&application, toast_message);
    Ok(get_project_health(application, project_identifier))
}

async fn run_dispatch_idle_issue_action(
    application: &AppHandle,
    project: &ProjectItem,
    repository: String,
    issue_number: u32,
    action: DispatchIdleIssueAction,
) -> Result<String, String> {
    let project_id = project.id;
    let project_name = project.name.clone();
    if action == DispatchIdleIssueAction::RetryAttempt {
        crate::state::try_with_store(application, move |store| {
            store
                .clear_dispatch_attempt(project_id, &repository, issue_number)
                .map_err(|error| error.to_string())?;
            Ok(())
        })
        .await?;
        return Ok(format!(
            "{project_name}: cleared attempt record for #{issue_number}; continuous dispatch will try again"
        ));
    }
    let host = project.effective_issue_host();
    let repo_dir = Path::new(&project.repository_path).to_path_buf();
    let fresh = tauri::async_runtime::spawn_blocking(move || {
        let client = IssueClient::for_host(&repo_dir, host, &repository)
            .map_err(|error| error.to_string())?;
        match action {
            DispatchIdleIssueAction::MarkNeedsOwner => client.mark_needs_owner(issue_number),
            DispatchIdleIssueAction::LiftHold => client.release_person_hold(issue_number),
            DispatchIdleIssueAction::RetryAttempt => Ok(()),
        }
        .map_err(|error| error.to_string())?;
        client.get(issue_number).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    crate::continuous_dispatch::update_cached_issue_and_refresh(application, project_id, fresh);
    Ok(match action {
        DispatchIdleIssueAction::MarkNeedsOwner => {
            format!("{project_name}: labelled #{issue_number} needs-owner")
        }
        DispatchIdleIssueAction::LiftHold => {
            format!("{project_name}: lifted hold on #{issue_number}")
        }
        DispatchIdleIssueAction::RetryAttempt => String::new(),
    })
}

/// `copy_github_issue_to_collapse` (§3.1): re-runs Collapse's idempotent GitHub issue
/// import to bring a missing GitHub issue onto Collapse with its original number, then
/// immediately retries pending land comments, closes or reverts for that issue (#614).
#[tauri::command]
#[specta::specta]
pub async fn copy_github_issue_to_collapse(
    application: AppHandle,
    project_identifier: u32,
    issue_number: u32,
) -> Result<ProjectHealthView, String> {
    let subject = crate::collapse_mirror::mirror_subject(&application, project_identifier)?;
    let collapse_repository = subject
        .repository
        .clone()
        .ok_or("This project does not mirror to Collapse")?;
    let (owner, name) = collapse_repository
        .split_once('/')
        .ok_or("Invalid Collapse repository name")?;
    let github_repository = crate::collapse_issue_sync::resolve_sync_github_repository(
        &application,
        project_identifier,
        &subject.repository_path,
        &collapse_repository,
    )
    .await?
    .ok_or("No GitHub repository is configured for this project")?;
    let token = tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?
        .ok_or("Save a Collapse personal access token with the 'issues' scope in Collapse settings first")?;
    let status = velocity_collapse::import_github_issues_and_wait(
        &token,
        owner,
        name,
        &github_repository,
        None,
    )
    .await
    .map_err(|failure| failure.message)?;
    verify_copied_collapse_issue(&subject.repository_path, &collapse_repository, issue_number)
        .await?;
    finish_copied_github_issue(
        &application,
        project_identifier,
        &subject.name,
        &collapse_repository,
        &github_repository,
        issue_number,
        &status,
    )
    .await?;
    Ok(get_project_health(application, project_identifier))
}

async fn finish_copied_github_issue(
    application: &AppHandle,
    project_identifier: u32,
    project_name: &str,
    collapse_repository: &str,
    github_repository: &str,
    issue_number: u32,
    status: &velocity_collapse::GitHubIssueImportStatus,
) -> Result<(), String> {
    velocity_github::IssueClient::invalidate_open_issues_for(
        IssueHost::Collapse,
        collapse_repository,
    );
    mark_collapse_issues_imported(application, project_identifier, collapse_repository);
    let high_water = status
        .result
        .as_ref()
        .filter(|result| result.next_number > 1)
        .and_then(|result| u32::try_from(result.next_number.saturating_sub(1)).ok());
    let _ = crate::collapse_issue_sync::enable_post_switch_sync(
        application,
        project_identifier,
        github_repository,
        high_water,
        status.finished_without_error_at(),
        true,
    )
    .await;
    crate::state::try_with_store(application, move |store| {
        store
            .clear_host_issue_problem(project_identifier, issue_number)
            .map_err(|error| error.to_string())?;
        Ok(())
    })
    .await?;
    sync_host_issue_problems(application, project_identifier);
    crate::rosen_events::run_follow_ups(application, project_identifier).await;
    sync_host_issue_problems(application, project_identifier);
    let message = format!(
        "{project_name}: copied #{issue_number} from GitHub to Collapse and retried pending updates"
    );
    crate::diagnostics::record(SOURCE, &message);
    let _ = crate::collapse_mirror::toast(application, message);
    Ok(())
}

async fn verify_copied_collapse_issue(
    repository_path: &Path,
    collapse_repository: &str,
    issue_number: u32,
) -> Result<(), String> {
    let dir = repository_path.to_path_buf();
    let repo = collapse_repository.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let client = IssueClient::for_collapse(&dir, &repo).map_err(|error| error.to_string())?;
        client.get(issue_number).map(|_| ()).map_err(|error| {
            format!("Issue #{issue_number} is still missing on Collapse after import: {error}")
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// `dismiss_host_issue_problem` (§3.1): clears a missing or mismatched host-issue health
/// alert and drops stuck pending updates for that issue number (#614).
#[tauri::command]
#[specta::specta]
pub async fn dismiss_host_issue_problem(
    application: AppHandle,
    project_identifier: u32,
    issue_number: u32,
) -> Result<ProjectHealthView, String> {
    crate::state::try_with_store(&application, move |store| {
        store
            .dismiss_host_issue_problem(project_identifier, issue_number)
            .map_err(|error| error.to_string())?;
        Ok(())
    })
    .await?;
    sync_host_issue_problems(&application, project_identifier);
    Ok(get_project_health(application, project_identifier))
}

/// `resolve_project_host_change` (§3.1): switches a project back to its previous issue host
/// or base CI source in one click, or acknowledges the current setting and clears the health
/// warning (#613).
#[tauri::command]
#[specta::specta]
pub async fn resolve_project_host_change(
    application: AppHandle,
    project_identifier: u32,
    switch_back: bool,
) -> Result<ProjectHealthView, String> {
    let (resolution, refresh) = crate::state::try_with_store(&application, move |store| {
        let resolution = store
            .resolve_host_source_change(project_identifier, switch_back)
            .map_err(|error| error.to_string())?;
        let refresh = matches!(
            resolution,
            velocity_store::rosen_mirror::HostChangeResolution::Resolved(_)
        )
        .then(|| store.layout_refresh());
        Ok((resolution, refresh))
    })
    .await?;
    match resolution {
        velocity_store::rosen_mirror::HostChangeResolution::CannotSwitchBack(reason) => Err(reason),
        velocity_store::rosen_mirror::HostChangeResolution::NothingPending => {
            sync_host_source_change(&application, project_identifier);
            Ok(get_project_health(application, project_identifier))
        }
        velocity_store::rosen_mirror::HostChangeResolution::Resolved(outcome) => {
            finish_resolved_host_change(
                &application,
                project_identifier,
                switch_back,
                &outcome,
                refresh,
            )
            .await?;
            Ok(get_project_health(application, project_identifier))
        }
    }
}

async fn finish_resolved_host_change(
    application: &AppHandle,
    project: u32,
    switch_back: bool,
    outcome: &velocity_store::rosen_mirror::ResolvedHostChange,
    refresh: Option<velocity_store::projects::LayoutRefresh>,
) -> Result<(), String> {
    if let Some(refresh) = refresh {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::layout::apply_refresh_and_publish(&handle, refresh)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    if outcome.issue_host_changed {
        match outcome.effective_issue_host {
            IssueHost::Collapse => {
                if let Some(repo) = outcome.collapse_repository.as_deref() {
                    velocity_github::IssueClient::invalidate_open_issues_for(
                        IssueHost::Collapse,
                        repo,
                    );
                    if let Some(item) = persisted_project(application, project) {
                        let _ = crate::collapse_issue_sync::enable_sync_from_import_status(
                            application,
                            project,
                            Path::new(&item.repository_path),
                            repo,
                            true,
                        )
                        .await;
                    }
                }
                recheck_collapse_import_soon(application, project);
            }
            IssueHost::GitHub => {
                clear_collapse_issues_not_imported(application, project);
            }
        }
    }
    sync_host_source_change(application, project);
    sync_host_issue_problems(application, project);
    crate::base_ci::wake(application);
    let host_label = match outcome.effective_issue_host {
        IssueHost::Collapse => "Collapse",
        IssueHost::GitHub => "GitHub",
    };
    let ci_label = match outcome.effective_ci_source {
        BaseCiSource::Rosen => "Rosen checkpoints",
        BaseCiSource::GitHub => "GitHub Actions",
    };
    let verb = if switch_back {
        "switched back to"
    } else {
        "kept"
    };
    let message = format!(
        "{}: {verb} {host_label} issues and {ci_label}",
        outcome.project_name
    );
    crate::diagnostics::record("collapse", &message);
    let _ = crate::collapse_mirror::toast(application, message);
    Ok(())
}

/// Immediately synchronizes the non-owner host/CI source change row for `project` (#613).
pub(crate) fn sync_host_source_change(application: &AppHandle, project: u32) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let next =
        persisted_project(application, project).and_then(|item| check_host_source_change(&item));
    let updated = {
        let mut reg = registry(&managed);
        let list = reg.views.entry(project).or_default();
        match next {
            Some(item) => {
                let changed = list
                    .iter()
                    .find(|existing| existing.key == HOST_SOURCE_CHANGED_KEY)
                    != Some(&item);
                upsert_item(list, item);
                changed.then(|| list.clone())
            }
            None => {
                let before = list.len();
                list.retain(|existing| existing.key != HOST_SOURCE_CHANGED_KEY);
                (list.len() != before).then(|| list.clone())
            }
        }
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

fn check_host_source_change(project: &ProjectItem) -> Option<ProjectHealthItem> {
    let change = project.host_source_change.as_ref()?;
    host_source_change_item(
        change,
        project.collapse_repository.is_some(),
        project.github_remote.is_some(),
    )
}

/// Immediately synchronizes missing/mismatched host-issue health rows for `project` (#614).
pub(crate) fn sync_host_issue_problems(application: &AppHandle, project: u32) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let next = persisted_project(application, project)
        .map(|item| check_host_issue_problems(&item))
        .unwrap_or_default();
    let updated = {
        let mut reg = registry(&managed);
        let list = reg.views.entry(project).or_default();
        let before = list.clone();
        list.retain(|existing| {
            !matches!(
                existing.kind,
                HealthSignalKind::HostIssueMissing | HealthSignalKind::HostIssueMismatch
            )
        });
        for item in next {
            upsert_item(list, item);
        }
        (*list != before).then(|| list.clone())
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

fn check_host_issue_problems(project: &ProjectItem) -> Vec<ProjectHealthItem> {
    let active_host = project.effective_issue_host();
    project
        .host_issue_problems
        .values()
        .filter(|problem| problem.host == active_host)
        .map(host_issue_problem_item)
        .collect()
}

/// Immediately synchronizes reopened-already-landed health rows for `project` (#615).
pub(crate) fn sync_reopened_landed_holds(application: &AppHandle, project: u32) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let next = persisted_project(application, project)
        .map(|item| check_reopened_landed_holds(&item))
        .unwrap_or_default();
    let updated = {
        let mut reg = registry(&managed);
        let list = reg.views.entry(project).or_default();
        let before = list.clone();
        list.retain(|existing| existing.kind != HealthSignalKind::ReopenedAlreadyLanded);
        for item in next {
            upsert_item(list, item);
        }
        (*list != before).then(|| list.clone())
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

fn check_reopened_landed_holds(project: &ProjectItem) -> Vec<ProjectHealthItem> {
    let active_host = project.effective_issue_host();
    project
        .continuous_dispatch
        .reopened_landed_holds
        .values()
        .map(|hold| {
            let url = match hold.issue.host.unwrap_or(active_host) {
                IssueHost::Collapse => {
                    velocity_core::collapse::issue_page(&hold.issue.repository, hold.issue.number)
                }
                IssueHost::GitHub => Some(format!(
                    "https://github.com/{}/issues/{}",
                    hold.issue.repository, hold.issue.number
                )),
            };
            reopened_already_landed_item(ReopenedLandedNotice {
                issue_number: hold.issue.number,
                commit: &hold.commit,
                land_summary: &hold.land_summary,
                issue_title: hold.issue.title.as_deref(),
                issue_url: url.as_deref(),
                reopened_by_import: hold.reopened_by_import,
                detected_at_ms: hold.detected_at_ms,
            })
        })
        .collect()
}

/// Whether project health already observed a finished Collapse issue import for `collapse_repository` (#615).
pub(crate) fn collapse_import_finished(
    application: &AppHandle,
    project: u32,
    collapse_repository: &str,
) -> bool {
    with_tracker(application, project, |tracker| {
        tracker.collapse_import.as_ref().is_some_and(|entry| {
            entry.import_finished
                && entry
                    .collapse_repository
                    .eq_ignore_ascii_case(collapse_repository)
        })
    })
}

fn persisted_project(application: &AppHandle, project_identifier: u32) -> Option<ProjectItem> {
    let store = application.try_state::<crate::state::ManagedStateStore>()?;
    let store = crate::threads::lock(store.inner(), "persisted state").ok()?;
    store.project(project_identifier).cloned()
}

/// Records that a post-land check was stopped for running too long (#486, #509).
pub(crate) fn record_post_land_stopped(
    application: &AppHandle,
    project: u32,
    commit: &str,
    command: &str,
    stopped: &StoppedCheck,
) {
    let now_ms = unix_millis();
    let detail = format_stopped_check_detail(
        command,
        stopped.test.as_deref(),
        stopped.process.as_deref(),
        &stopped.reason,
    );
    crate::diagnostics::record(
        "post-land",
        &format!(
            "Project {project} post-land check stopped on {}: {detail}",
            short_commit(commit)
        ),
    );
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let item = stopped_check_item(commit, command, stopped, now_ms);
    let updated = {
        let mut reg = registry(&managed);
        reg.projects.entry(project).or_default().stopped_check = Some(StoppedCheckRecord {
            commit: commit.to_owned(),
            command: command.to_owned(),
            stopped: stopped.clone(),
            acted_at_ms: now_ms,
        });
        let list = reg.views.entry(project).or_default();
        upsert_item(list, item);
        list.clone()
    };
    emit_view(application, project, updated);
}

/// Prunes parked issue records for issues no longer labelled `no-change`, keeping recent
/// automatic resolutions briefly visible on the health strip (#509).
pub(crate) fn retain_parked_issues(application: &AppHandle, project: u32, active: &[u32]) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let now = unix_seconds();
    let mut reg = registry(&managed);
    if let Some(tracker) = reg.projects.get_mut(&project) {
        tracker.parked.retain(|number, record| match record {
            ParkedRecord::Resolved { expires_secs, .. } => *expires_secs > now,
            ParkedRecord::Manual { .. } | ParkedRecord::Failed { .. } => active.contains(number),
        });
    }
}

pub(crate) fn record_parked_resolved(
    application: &AppHandle,
    project: u32,
    issue: u32,
    action: &str,
) {
    let now_ms = unix_millis();
    let now = now_ms / 1000;
    crate::diagnostics::record(SOURCE, &format!("Project {project} #{issue}: {action}"));
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let record = ParkedRecord::Resolved {
        action: action.to_owned(),
        acted_at_ms: now_ms,
        expires_secs: now.saturating_add(RECENT_ACTION_TTL_SECS),
    };
    let item = parked_item(issue, &record, now);
    let updated = {
        let mut reg = registry(&managed);
        reg.projects
            .entry(project)
            .or_default()
            .parked
            .insert(issue, record);
        if let Some(item) = item {
            let list = reg.views.entry(project).or_default();
            upsert_item(list, item);
            Some(list.clone())
        } else {
            None
        }
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

pub(crate) fn record_parked_failure(
    application: &AppHandle,
    project: u32,
    issue: u32,
    error: &str,
) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let now_ms = unix_millis();
    let record = ParkedRecord::Failed {
        error: error.to_owned(),
        acted_at_ms: now_ms,
    };
    let item = parked_item(issue, &record, now_ms / 1000);
    let updated = {
        let mut reg = registry(&managed);
        reg.projects
            .entry(project)
            .or_default()
            .parked
            .insert(issue, record);
        if let Some(item) = item {
            let list = reg.views.entry(project).or_default();
            upsert_item(list, item);
            Some(list.clone())
        } else {
            None
        }
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

pub(crate) fn record_parked_manual(application: &AppHandle, project: u32, issue: u32) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let now = unix_seconds();
    let mut reg = registry(&managed);
    reg.projects
        .entry(project)
        .or_default()
        .parked
        .entry(issue)
        .or_insert(ParkedRecord::Manual { seen_secs: now });
}

/// Records stale local AIRC claims that Velocity released so project health reports
/// `"stale AIRC claim by <nick>, released"` rather than `"claimed in AIRC"` (#566).
pub(crate) fn record_stale_claim_released(
    application: &AppHandle,
    project: u32,
    project_name: &str,
    reaped: &[velocity_airc::ReapedClaim],
) {
    if reaped.is_empty() {
        return;
    }
    let now_ms = unix_millis();
    let now = now_ms / 1000;
    let mut items = Vec::new();
    for claim in reaped {
        let reason = stale_claim_reason(&claim.nick);
        let label = match issue_number_from_piece(&claim.piece) {
            Some(number) => format!("#{number}"),
            None => claim.piece.clone(),
        };
        crate::diagnostics::record(SOURCE, &format!("{project_name}: {label} ({reason})"));
        let item = ProjectHealthItem::new(
            format!("stale-claim:{}", claim.piece),
            HealthSignalKind::DispatchIdle,
            HealthSeverity::Info,
            format!("{label} had a stale AIRC claim"),
            reason,
            now_ms,
        );
        record_recent_action(application, project, item.clone(), now);
        items.push(item);
    }
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let updated = {
        let mut reg = registry(&managed);
        let list = reg.views.entry(project).or_default();
        for item in items {
            upsert_item(list, item);
        }
        list.clone()
    };
    emit_view(application, project, updated);
}

/// Queues a release cut that started an integration build so Velocity dispatches
/// `release.yml` automatically once `commit` has a green `velocity/integration` status (#521).
pub(crate) fn queue_pending_release(
    application: &AppHandle,
    project: u32,
    tag: String,
    commit: String,
) {
    with_tracker(application, project, |tracker| {
        tracker.pending_release = Some(PendingRelease { tag, commit });
        tracker.release_health = None;
    });
}

/// Clears the cached GitHub release/manifest health check for `project` (#521).
pub(crate) fn invalidate_release_health(application: &AppHandle, project: u32) {
    with_tracker(application, project, |tracker| {
        tracker.release_health = None;
    });
}

/// Records that Collapse could not automatically configure GitHub forwarding and publishes
/// the "GitHub copy is not being updated: <reason>" project health row (#579, #611).
pub(crate) fn record_collapse_forwarding_needed(
    application: &AppHandle,
    project: u32,
    collapse_repository: &str,
    github_repository: &str,
    reason: &str,
    opened_setup_page: bool,
) {
    let now_ms = unix_millis();
    let record = CollapseForwardingRecord {
        collapse_repository: collapse_repository.to_owned(),
        github_repository: github_repository.to_owned(),
        reason: reason.to_owned(),
        opened_setup_page,
        acted_at_ms: now_ms,
    };
    let item = collapse_forwarding_item(
        Some(collapse_repository),
        Some(github_repository),
        None,
        None,
        Some(&record),
        now_ms,
    );
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let updated = {
        let mut reg = registry(&managed);
        reg.projects.entry(project).or_default().collapse_forwarding = Some(record);
        let list = reg.views.entry(project).or_default();
        upsert_item(list, item);
        list.clone()
    };
    emit_view(application, project, updated);
}

/// Clears any pending Collapse forwarding health row for `project` (#579).
pub(crate) fn clear_collapse_forwarding(application: &AppHandle, project: u32) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let updated = {
        let mut reg = registry(&managed);
        if let Some(tracker) = reg.projects.get_mut(&project) {
            tracker.collapse_forwarding = None;
        }
        let list = reg.views.entry(project).or_default();
        let before = list.len();
        list.retain(|item| item.key != COLLAPSE_FORWARDING_KEY);
        (list.len() != before).then(|| list.clone())
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

/// Synchronizes the Collapse forwarding health row immediately after Collapse's forwarder
/// state or the project's GitHub repository changes, clearing it once in sync (#579, #611).
pub(crate) fn sync_collapse_forwarding(
    application: &AppHandle,
    project: u32,
    collapse_repository: Option<&str>,
    github_remote: Option<&str>,
    forwarder: Option<ForwarderState>,
    forwarder_message: Option<&str>,
) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let now_ms = unix_millis();
    let updated = {
        let mut reg = registry(&managed);
        let record = reg
            .projects
            .get(&project)
            .and_then(|tracker| tracker.collapse_forwarding.clone());
        let needs_attention = collapse_forwarding_needs_attention(
            collapse_repository.is_some(),
            github_remote.is_some(),
            forwarder,
            record.is_some(),
        );
        if !needs_attention {
            if let Some(tracker) = reg.projects.get_mut(&project) {
                tracker.collapse_forwarding = None;
            }
            let list = reg.views.entry(project).or_default();
            let before = list.len();
            list.retain(|item| item.key != COLLAPSE_FORWARDING_KEY);
            (list.len() != before).then(|| list.clone())
        } else {
            let item = collapse_forwarding_item(
                collapse_repository,
                github_remote,
                forwarder,
                forwarder_message,
                record.as_ref(),
                now_ms,
            );
            let list = reg.views.entry(project).or_default();
            let changed = list
                .iter()
                .find(|entry| entry.key == COLLAPSE_FORWARDING_KEY)
                != Some(&item);
            upsert_item(list, item);
            changed.then(|| list.clone())
        }
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

/// Marks Collapse GitHub issue import finished for `project` and clears the
/// "Issues on Collapse were never imported from GitHub" warning immediately (#612).
pub(crate) fn mark_collapse_issues_imported(
    application: &AppHandle,
    project: u32,
    collapse_repository: &str,
) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let now = unix_seconds();
    let updated = {
        let mut reg = registry(&managed);
        reg.projects.entry(project).or_default().collapse_import = Some(CollapseImportCache {
            collapse_repository: collapse_repository.to_owned(),
            checked_at_secs: now,
            import_finished: true,
            running: false,
            github_repository: None,
            collapse_issue_count: None,
            last_error: None,
        });
        let list = reg.views.entry(project).or_default();
        let before = list.len();
        list.retain(|item| item.key != COLLAPSE_ISSUES_NOT_IMPORTED_KEY);
        (list.len() != before).then(|| list.clone())
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

/// Clears the Collapse issue import health row (for example when switching back to GitHub) (#612).
pub(crate) fn clear_collapse_issues_not_imported(application: &AppHandle, project: u32) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let updated = {
        let mut reg = registry(&managed);
        if let Some(tracker) = reg.projects.get_mut(&project) {
            tracker.collapse_import = None;
        }
        let list = reg.views.entry(project).or_default();
        let before = list.len();
        list.retain(|item| item.key != COLLAPSE_ISSUES_NOT_IMPORTED_KEY);
        (list.len() != before).then(|| list.clone())
    };
    if let Some(items) = updated {
        emit_view(application, project, items);
    }
}

/// Records the latest failed `import_collapse_issues` attempt on the active health row (#612).
pub(crate) fn record_collapse_import_error(
    application: &AppHandle,
    project: u32,
    collapse_repository: &str,
    error: &str,
) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let now_ms = unix_millis();
    let now = now_ms / 1000;
    let updated = {
        let mut reg = registry(&managed);
        let tracker = reg.projects.entry(project).or_default();
        let mut cache = tracker
            .collapse_import
            .clone()
            .filter(|entry| {
                entry
                    .collapse_repository
                    .eq_ignore_ascii_case(collapse_repository)
            })
            .unwrap_or(CollapseImportCache {
                collapse_repository: collapse_repository.to_owned(),
                checked_at_secs: now,
                import_finished: false,
                running: false,
                github_repository: None,
                collapse_issue_count: Some(0),
                last_error: None,
            });
        cache.last_error = Some(error.to_owned());
        let item = collapse_issues_not_imported_item(&cache, now_ms);
        tracker.collapse_import = Some(cache);
        let list = reg.views.entry(project).or_default();
        upsert_item(list, item);
        list.clone()
    };
    emit_view(application, project, updated);
}

/// Invalidates cached Collapse import status for `project` and checks it right away (#612).
pub(crate) fn recheck_collapse_import_soon(application: &AppHandle, project: u32) {
    with_tracker(application, project, |tracker| {
        tracker.collapse_import = None;
    });
    spawn_collapse_import_check(application, project);
}

/// Triggers an immediate Collapse import status check when switching to a Collapse-hosted
/// project whose import status has not been checked yet (#612).
pub(crate) fn recheck_collapse_import_if_unchecked(application: &AppHandle, project: u32) {
    let unchecked = with_tracker(application, project, |tracker| {
        tracker.collapse_import.is_none()
    });
    if unchecked {
        spawn_collapse_import_check(application, project);
    }
}

fn spawn_collapse_import_check(application: &AppHandle, project: u32) {
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        let item = crate::state::with_store(&handle, move |store| store.project(project).cloned())
            .await
            .ok()
            .flatten();
        let Some(project_item) = item else {
            return;
        };
        if project_item.effective_issue_host() != IssueHost::Collapse {
            clear_collapse_issues_not_imported(&handle, project);
            return;
        }
        let now_ms = unix_millis();
        let now = now_ms / 1000;
        let next = check_collapse_issues_import(&handle, &project_item, now, now_ms).await;
        let Some(managed) = handle.try_state::<ManagedProjectHealth>() else {
            return;
        };
        let updated = {
            let mut reg = registry(&managed);
            let list = reg.views.entry(project).or_default();
            match next {
                Some(entry) => {
                    let changed = list
                        .iter()
                        .find(|existing| existing.key == COLLAPSE_ISSUES_NOT_IMPORTED_KEY)
                        != Some(&entry);
                    upsert_item(list, entry);
                    changed.then(|| list.clone())
                }
                None => {
                    let before = list.len();
                    list.retain(|existing| existing.key != COLLAPSE_ISSUES_NOT_IMPORTED_KEY);
                    (list.len() != before).then(|| list.clone())
                }
            }
        };
        if let Some(items) = updated {
            emit_view(&handle, project, items);
        }
    });
}

async fn check_all_projects(application: &AppHandle) {
    let Ok(app_data) = application.path().app_data_dir() else {
        return;
    };
    let projects = rosen_projects(application).await;
    for project in projects {
        check_project(application, &app_data, &project).await;
    }
}

async fn rosen_projects(application: &AppHandle) -> Vec<ProjectItem> {
    crate::state::with_store(application, |store| {
        store
            .state()
            .projects
            .iter()
            .filter(|project| project.rosen_in_use())
            .cloned()
            .collect()
    })
    .await
    .unwrap_or_default()
}

async fn check_project(application: &AppHandle, app_data: &Path, project: &ProjectItem) {
    let server = ProjectServer::new(app_data, project.id);
    let now_ms = unix_millis();
    let now = now_ms / 1000;
    let dispatch = crate::continuous_dispatch::health_snapshot(application, project.id, now)
        .unwrap_or_default();
    let mut items = Vec::new();
    if let Some(item) =
        check_collapse_proactive_auth(application, project, &server, now, now_ms).await
    {
        items.push(item);
    }
    if let Some(item) = check_collapse_forwarding(application, project, now_ms) {
        items.push(item);
    }
    if let Some(item) = check_collapse_issues_import(application, project, now, now_ms).await {
        items.push(item);
    }
    if let Some(item) = check_host_source_change(project) {
        items.push(item);
    }
    items.extend(check_host_issue_problems(project));
    items.extend(check_reopened_landed_holds(project));
    if let Some(item) = check_mirror(application, project, &server, now, now_ms).await {
        items.push(item);
    }
    let last_outcome = read_last_outcome(&server).await;
    if let Some(item) = check_post_land_stalled(
        application,
        project,
        &server,
        last_outcome.as_deref(),
        now,
        now_ms,
    )
    .await
    {
        items.push(item);
    }
    if let Some(item) = check_integration_uncovered(
        application,
        project,
        &server,
        last_outcome.as_deref(),
        now,
        now_ms,
    )
    .await
    {
        items.push(item);
    }
    items.extend(check_base_red(application, project, &server, now_ms).await);
    if let Some(item) = check_landed_waiting(&dispatch, last_outcome.as_deref(), now_ms) {
        items.push(item);
    }
    items.extend(collect_parked_items(application, project.id, now));
    items.extend(check_review_stalled(application, project, &server, now, now_ms).await);
    if let Some(item) = check_dispatch_idle(application, project, &dispatch, now, now_ms) {
        items.push(item);
    }
    if let Some(item) = check_post_land_stopped(application, project.id, &server).await {
        items.push(item);
    }
    if let Some(item) = check_airc_issue_tool_health(app_data, project.id, now_ms) {
        items.push(item);
    }
    if let Some(item) = check_local_build_restart_health(application, project, now_ms) {
        items.push(item);
    }
    crate::rosen_deploy::retry_setup_failures(application, project.id);
    items.extend(check_deploy_health(application, project, &server, now_ms).await);
    items.extend(check_release_health(application, project, &server, now, now_ms).await);
    items.extend(collect_recent_actions(application, project.id, now, &items));
    stabilize_and_publish(application, project.id, items, now_ms);
}

/// Records that a `[deploy.<step>]` command started running on `commit` (#594).
pub(crate) fn record_deploy_running(
    application: &AppHandle,
    project: u32,
    step: &str,
    commit: &str,
) {
    let now_ms = unix_millis();
    let item = ProjectHealthItem::new(
        deploy_health_key(step),
        HealthSignalKind::Deploy,
        HealthSeverity::Info,
        format!("Deploying {step} at {}", short_commit(commit)),
        "Running [deploy] step in base checkout",
        now_ms,
    );
    upsert_health_item(application, project, item);
}

/// Records that a `[deploy.<step>]` command passed on `commit` (#594, #604).
pub(crate) fn record_deploy_passed(
    application: &AppHandle,
    project: u32,
    step: &str,
    commit: &str,
    secret_sources: Option<&str>,
) {
    let now_ms = unix_millis();
    let now = now_ms / 1000;
    let action = match secret_sources
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(sources) => format!("Deploy step passed · {sources}"),
        None => "Deploy step passed".to_owned(),
    };
    let item = ProjectHealthItem::new(
        deploy_health_key(step),
        HealthSignalKind::Deploy,
        HealthSeverity::Info,
        format!("Deployed {step} at {}", short_commit(commit)),
        action,
        now_ms,
    );
    record_recent_action(application, project, item.clone(), now);
    upsert_health_item(application, project, item);
}

/// Records that a `[deploy.<step>]` command failed on `commit` (#594).
pub(crate) fn record_deploy_failed(
    application: &AppHandle,
    project: u32,
    step: &str,
    commit: &str,
    action: &str,
    detail: Option<String>,
    thread: Option<u32>,
) {
    let now_ms = unix_millis();
    let item = ProjectHealthItem::new(
        deploy_health_key(step),
        HealthSignalKind::Deploy,
        HealthSeverity::Error,
        format!("Deploy `{step}` failed at {}", short_commit(commit)),
        action,
        now_ms,
    )
    .with_detail(detail)
    .with_thread(thread);
    upsert_health_item(application, project, item);
}

/// Records that a `[deploy.<step>]` step hit a setup error and will retry once on the next pass (#606).
pub(crate) fn record_deploy_retrying(
    application: &AppHandle,
    project: u32,
    step: &str,
    commit: &str,
    error: &str,
) {
    let now_ms = unix_millis();
    upsert_health_item(
        application,
        project,
        deploy_retrying_health_item(step, commit, error, now_ms),
    );
}

fn deploy_retrying_health_item(
    step: &str,
    commit: &str,
    error: &str,
    acted_at: u64,
) -> ProjectHealthItem {
    ProjectHealthItem::new(
        deploy_health_key(step),
        HealthSignalKind::Deploy,
        HealthSeverity::Warning,
        format!("Deploy `{step}` setup error at {}", short_commit(commit)),
        "Retrying once on the next pass",
        acted_at,
    )
    .with_detail(velocity_core::deploy::summarize_output(error, 180))
}

/// Records that a `[deploy.<step>]` step was skipped (for example missing secrets) (#594).
pub(crate) fn record_deploy_skipped(
    application: &AppHandle,
    project: u32,
    step: &str,
    commit: &str,
    reason: &str,
) {
    let now_ms = unix_millis();
    let missing_secrets = reason.starts_with(velocity_core::deploy::MISSING_SECRETS_PREFIX);
    let (signal, action) = if missing_secrets {
        (
            format!("Deploy `{step}` skipped: missing secrets"),
            "Set the required secrets in Settings -> Workspaces -> Deploy secrets",
        )
    } else {
        (
            format!("Deploy `{step}` skipped at {}", short_commit(commit)),
            "Deploy step was skipped",
        )
    };
    let item = ProjectHealthItem::new(
        deploy_health_key(step),
        HealthSignalKind::Deploy,
        HealthSeverity::Warning,
        signal,
        action,
        now_ms,
    )
    .with_detail(velocity_core::deploy::summarize_output(reason, 180));
    upsert_health_item(application, project, item);
}

fn deploy_health_key(step: &str) -> String {
    format!("deploy:{step}")
}

/// Immediately reflects a failed local-build restart on Velocity's own project while at
/// least one issue is waiting for a restart, and clears the row once resolved (#605).
pub(crate) fn sync_local_build_restart_health(application: &AppHandle) {
    let Some(project_id) = velocity_rosen_project_id(application) else {
        return;
    };
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let now_ms = unix_millis();
    let item = local_build_restart_health_item(application, now_ms);
    let updated = {
        let mut reg = registry(&managed);
        let list = reg.views.entry(project_id).or_default();
        match item {
            Some(item) => {
                let changed = list
                    .iter()
                    .find(|entry| entry.key == LOCAL_BUILD_RESTART_FAILED_KEY)
                    .is_none_or(|existing| {
                        existing.signal != item.signal
                            || existing.action != item.action
                            || existing.detail != item.detail
                    });
                if changed {
                    upsert_item(list, item);
                    Some(list.clone())
                } else {
                    None
                }
            }
            None => {
                let before = list.len();
                list.retain(|entry| entry.key != LOCAL_BUILD_RESTART_FAILED_KEY);
                (list.len() != before).then(|| list.clone())
            }
        }
    };
    if let Some(items) = updated {
        emit_view(application, project_id, items);
    }
}

fn velocity_rosen_project_id(application: &AppHandle) -> Option<u32> {
    let store = application.try_state::<crate::state::ManagedStateStore>()?;
    let store = store.lock().ok()?;
    store
        .state()
        .projects
        .iter()
        .find(|project| is_velocity_rosen_project(project))
        .map(|project| project.id)
}

fn is_velocity_rosen_project(project: &ProjectItem) -> bool {
    project.rosen_in_use()
        && project.github_remote.as_deref().is_some_and(|remote| {
            remote.eq_ignore_ascii_case(velocity_github::velocity_reports::VELOCITY_REPOSITORY)
        })
}

fn check_local_build_restart_health(
    application: &AppHandle,
    project: &ProjectItem,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    if !is_velocity_rosen_project(project) {
        return None;
    }
    local_build_restart_health_item(application, now_ms)
}

fn local_build_restart_health_item(
    application: &AppHandle,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let failure = crate::local_build::restart_failure_health(application)?;
    let issues = if failure.waiting_issues == 1 {
        "1 issue waiting for restart".to_owned()
    } else {
        format!("{} issues waiting for restart", failure.waiting_issues)
    };
    let signal = match failure.commit.as_deref() {
        Some(commit) => format!(
            "Restart into {} failed ({issues})",
            velocity_updates::local_build::short_commit(commit)
        ),
        None => format!("Local build restart failed ({issues})"),
    };
    let action = if failure.auto_restart {
        "Auto-restart will retry after the next local build or in 10 min".to_owned()
    } else {
        "Auto-restart is off; use Restart in the title bar".to_owned()
    };
    let detail = if failure.waiting_detail.is_empty() {
        Some(failure.reason)
    } else {
        Some(format!("{} · {}", failure.reason, failure.waiting_detail))
    };
    Some(
        ProjectHealthItem::new(
            LOCAL_BUILD_RESTART_FAILED_KEY,
            HealthSignalKind::LocalBuildRestartFailed,
            HealthSeverity::Error,
            signal,
            action,
            now_ms,
        )
        .with_detail(detail),
    )
}

fn upsert_health_item(application: &AppHandle, project: u32, item: ProjectHealthItem) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let updated = {
        let mut reg = registry(&managed);
        let list = reg.views.entry(project).or_default();
        upsert_item(list, item);
        list.clone()
    };
    emit_view(application, project, updated);
}

async fn check_deploy_health(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    now_ms: u64,
) -> Vec<ProjectHealthItem> {
    let reading = server.clone();
    let runs = tauri::async_runtime::spawn_blocking(move || {
        velocity_rosen::deploy::latest_deploys_per_step(&reading)
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default();
    runs.into_iter()
        .filter_map(|run| deploy_run_health_item(application, project, &run, now_ms))
        .collect()
}

fn deploy_run_health_item(
    application: &AppHandle,
    project: &ProjectItem,
    run: &velocity_rosen::deploy::DeployRunRecord,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let acted_at = run
        .submitted_at
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(now_ms);
    match run.state {
        velocity_core::deploy::DeployRunState::Running => {
            if let Some(retry) = crate::rosen_deploy::pending_setup_retry(
                application,
                project.id,
                &run.step,
                &run.commit,
            ) {
                return Some(deploy_retrying_health_item(
                    &run.step,
                    &run.commit,
                    &retry.error,
                    retry.failed_at_ms,
                ));
            }
            Some(ProjectHealthItem::new(
                deploy_health_key(&run.step),
                HealthSignalKind::Deploy,
                HealthSeverity::Info,
                format!("Deploying {} at {}", run.step, short_commit(&run.commit)),
                "Running [deploy] step in base checkout",
                acted_at,
            ))
        }
        velocity_core::deploy::DeployRunState::Failed => {
            Some(failed_deploy_health_item(project, run, acted_at))
        }
        velocity_core::deploy::DeployRunState::Skipped
            if run.superseded_by.is_none()
                && run
                    .output
                    .starts_with(velocity_core::deploy::MISSING_SECRETS_PREFIX) =>
        {
            Some(
                ProjectHealthItem::new(
                    deploy_health_key(&run.step),
                    HealthSignalKind::Deploy,
                    HealthSeverity::Warning,
                    format!("Deploy `{}` skipped: missing secrets", run.step),
                    "Set the required secrets in Settings -> Workspaces -> Deploy secrets",
                    acted_at,
                )
                .with_detail(velocity_core::deploy::summarize_output(&run.output, 180)),
            )
        }
        velocity_core::deploy::DeployRunState::Pending
        | velocity_core::deploy::DeployRunState::Passed
        | velocity_core::deploy::DeployRunState::Skipped => None,
    }
}

fn failed_deploy_health_item(
    project: &ProjectItem,
    run: &velocity_rosen::deploy::DeployRunRecord,
    acted_at: u64,
) -> ProjectHealthItem {
    let signal = format!(
        "Deploy `{}` failed at {}",
        run.step,
        short_commit(&run.commit)
    );
    let detail = velocity_core::deploy::summarize_deploy_error(&run.output, 180);
    if let Some(hint) = velocity_core::deploy::extract_cli_scope_hint(&run.output) {
        let action = hint.strip_prefix("[Velocity] ").unwrap_or(hint);
        return ProjectHealthItem::new(
            deploy_health_key(&run.step),
            HealthSignalKind::Deploy,
            HealthSeverity::Error,
            signal,
            action,
            acted_at,
        )
        .with_detail(detail);
    }
    let repair = project
        .deploy_repairs
        .iter()
        .rev()
        .find(|repair| !repair.closed && repair.step == run.step);
    let (action, thread) = match repair {
        Some(repair) => (
            "Fix-forward thread opened for deploy failure",
            Some(repair.thread),
        ),
        None if !project.spec_locked => (
            "Lock the spec to start a fix-forward thread for this deploy failure",
            None,
        ),
        None => ("Deploy step failed; awaiting fix-forward thread", None),
    };
    ProjectHealthItem::new(
        deploy_health_key(&run.step),
        HealthSignalKind::Deploy,
        HealthSeverity::Error,
        signal,
        action,
        acted_at,
    )
    .with_detail(detail)
    .with_thread(thread)
}

/// Signal: an AIRC issue tool failed locally for a reason that isn't GitHub's (#581).
fn check_airc_issue_tool_health(
    app_data: &Path,
    project: u32,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let failure = velocity_threads::issue_activity::read_issue_tool_failure(app_data, project)?;
    let ttl_ms = AIRC_ISSUE_FAILURE_TTL_SECONDS.saturating_mul(1000);
    if now_ms.saturating_sub(failure.failed_at_ms) > ttl_ms {
        velocity_threads::issue_activity::clear_issue_tool_failure(app_data, project);
        return None;
    }
    Some(airc_issue_tool_failure_item(&failure))
}

/// Signal: Collapse mirror reads need proactive auth (#571).
///
/// Private Collapse repositories answer unauthenticated `git ls-remote` with 404 and no
/// login challenge, so Rosen's mirror push tip check and Velocity's catch-up both need
/// `http.https://api.collapse.sh/.proactiveAuth = basic` (Git 2.46 or later).
async fn check_collapse_proactive_auth(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let (repository, mirror_url) = collapse_mirror_target(project, server).await?;
    let status = tauri::async_runtime::spawn_blocking(move || {
        velocity_collapse::check_proactive_auth(Some(&mirror_url))
    })
    .await
    .ok()?;
    match status {
        velocity_collapse::ProactiveAuthStatus::Configured => None,
        velocity_collapse::ProactiveAuthStatus::UnsupportedGit { reason } => Some(
            ProjectHealthItem::new(
                "collapse-proactive-auth",
                HealthSignalKind::CollapseProactiveAuth,
                HealthSeverity::Error,
                "Collapse mirror reads need proactive auth",
                reason,
                now_ms,
            )
            .with_detail(Some(format!(
                "{repository} · requires {} = basic",
                velocity_collapse::PROACTIVE_AUTH_KEY
            ))),
        ),
        velocity_collapse::ProactiveAuthStatus::Missing { current } => {
            remediate_collapse_proactive_auth(
                application,
                project,
                &repository,
                current.as_deref(),
                now,
                now_ms,
            )
            .await
        }
    }
}

async fn remediate_collapse_proactive_auth(
    application: &AppHandle,
    project: &ProjectItem,
    repository: &str,
    current: Option<&str>,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let set_result = tauri::async_runtime::spawn_blocking(velocity_collapse::ensure_proactive_auth)
        .await
        .map_err(|error| error.to_string())
        .and_then(|inner| inner.map_err(|error| error.to_string()));
    match set_result {
        Ok(()) => {
            let action = format!(
                "Set {} = basic in global git config",
                velocity_collapse::PROACTIVE_AUTH_KEY
            );
            crate::diagnostics::record(SOURCE, &format!("{}: {action}", project.name));
            record_recent_action(
                application,
                project.id,
                ProjectHealthItem::new(
                    "collapse-proactive-auth",
                    HealthSignalKind::CollapseProactiveAuth,
                    HealthSeverity::Info,
                    "Collapse mirror reads need proactive auth",
                    action,
                    now_ms,
                )
                .with_detail(Some(repository.to_owned())),
                now,
            );
            None
        }
        Err(error) => {
            let detail = match current {
                Some(value) => format!("{repository} · current value: {value}"),
                None => format!(
                    "{repository} · missing {} = basic",
                    velocity_collapse::PROACTIVE_AUTH_KEY
                ),
            };
            Some(
                ProjectHealthItem::new(
                    "collapse-proactive-auth",
                    HealthSignalKind::CollapseProactiveAuth,
                    HealthSeverity::Error,
                    "Collapse mirror reads need proactive auth",
                    format!(
                        "Could not set {} = basic: {error}",
                        velocity_collapse::PROACTIVE_AUTH_KEY
                    ),
                    now_ms,
                )
                .with_detail(Some(detail)),
            )
        }
    }
}

async fn collapse_mirror_target(
    project: &ProjectItem,
    server: &ProjectServer,
) -> Option<(String, String)> {
    if let Some(repository) = &project.collapse_repository {
        let remote = velocity_core::collapse::mirror_remote(repository)?;
        return Some((repository.clone(), remote));
    }
    let remote = velocity_rosen::mirror_remote(server).await.ok().flatten()?;
    let repository = velocity_core::collapse::repository_from_remote(&remote)?;
    Some((repository, remote))
}

/// Signal: GitHub copy is not being updated until Collapse downstream forwarding works (#579, #611).
fn check_collapse_forwarding(
    application: &AppHandle,
    project: &ProjectItem,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let health = project.collapse_mirror_health.as_ref();
    let forwarder = health.map(|item| item.forwarder);
    let forwarder_message = health.and_then(|item| item.message.as_deref());
    let record = with_tracker(application, project.id, |tracker| {
        tracker.collapse_forwarding.clone()
    });
    if !collapse_forwarding_needs_attention(
        project.collapse_repository.is_some(),
        project.github_remote.is_some(),
        forwarder,
        record.is_some(),
    ) {
        if record.is_some() {
            with_tracker(application, project.id, |tracker| {
                tracker.collapse_forwarding = None;
            });
        }
        return None;
    }
    Some(collapse_forwarding_item(
        project.collapse_repository.as_deref(),
        project.github_remote.as_deref(),
        forwarder,
        forwarder_message,
        record.as_ref(),
        now_ms,
    ))
}

fn collapse_forwarding_item(
    collapse_repository: Option<&str>,
    github_repository: Option<&str>,
    forwarder: Option<ForwarderState>,
    forwarder_message: Option<&str>,
    record: Option<&CollapseForwardingRecord>,
    now_ms: u64,
) -> ProjectHealthItem {
    let settings_repo = record
        .map(|entry| entry.collapse_repository.as_str())
        .or(collapse_repository);
    let (reason, action, detail, acted_at_ms) = match record {
        Some(entry) => {
            let reason = entry.reason.trim().to_owned();
            let action = if entry.opened_setup_page {
                "Opened setup page; set or finish the GitHub downstream in Collapse settings"
                    .to_owned()
            } else {
                "Set the GitHub downstream by hand in Collapse repository settings".to_owned()
            };
            let detail = Some(format!(
                "{} → {}",
                entry.collapse_repository, entry.github_repository
            ));
            (reason, action, detail, entry.acted_at_ms)
        }
        None => {
            let reason = match forwarder {
                Some(ForwarderState::Stopped) => forwarder_message
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .unwrap_or("Collapse forwarder stopped")
                    .to_owned(),
                _ => "Collapse has no downstream remote configured".to_owned(),
            };
            let detail = match (collapse_repository, github_repository) {
                (Some(collapse), Some(github)) => Some(format!("{collapse} → {github}")),
                (_, Some(github)) => Some(github.to_owned()),
                (Some(collapse), None) => Some(collapse.to_owned()),
                (None, None) => None,
            };
            (
                reason,
                "Set the GitHub downstream by hand in Collapse repository settings".to_owned(),
                detail,
                now_ms,
            )
        }
    };
    ProjectHealthItem::new(
        COLLAPSE_FORWARDING_KEY,
        HealthSignalKind::CollapseForwarding,
        HealthSeverity::Error,
        collapse_forwarding_signal(&reason),
        action,
        acted_at_ms,
    )
    .with_detail(detail)
    .with_action_link(
        settings_repo.and_then(downstream_settings_page),
        "Open Collapse settings",
    )
}

/// Signal: issues host is Collapse, but Collapse's GitHub issue import never finished (#612).
async fn check_collapse_issues_import(
    application: &AppHandle,
    project: &ProjectItem,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    if project.effective_issue_host() != IssueHost::Collapse {
        return None;
    }
    let collapse_repository = project.collapse_repository.as_deref()?;
    let cached = with_tracker(application, project.id, |tracker| {
        tracker.collapse_import.clone()
    });
    let cache = match cached {
        Some(entry)
            if entry
                .collapse_repository
                .eq_ignore_ascii_case(collapse_repository)
                && (entry.import_finished
                    || now.saturating_sub(entry.checked_at_secs)
                        < COLLAPSE_IMPORT_CHECK_COOLDOWN_SECS) =>
        {
            entry
        }
        previous => {
            let fresh = fetch_collapse_import_cache(project, collapse_repository, now).await;
            match fresh {
                Some(entry) => {
                    with_tracker(application, project.id, |tracker| {
                        tracker.collapse_import = Some(entry.clone());
                    });
                    entry
                }
                None => previous.filter(|entry| {
                    entry
                        .collapse_repository
                        .eq_ignore_ascii_case(collapse_repository)
                })?,
            }
        }
    };
    let never_finished = collapse_import_never_finished(cache.running, None, cache.import_finished);
    let warn = collapse_issues_import_warning_needed(
        project.effective_issue_host(),
        project.collapse_repository.is_some(),
        never_finished,
        cache.github_repository.is_some(),
        cache.collapse_issue_count,
    );
    warn.then(|| collapse_issues_not_imported_item(&cache, now_ms))
}

async fn fetch_collapse_import_cache(
    project: &ProjectItem,
    collapse_repository: &str,
    now: u64,
) -> Option<CollapseImportCache> {
    let (owner, name) = collapse_repository.split_once('/')?;
    let token = tauri::async_runtime::spawn_blocking(velocity_collapse::resolve_issues_token)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()?;
    let status = velocity_collapse::get_issue_import_status(&token, owner, name)
        .await
        .ok()?;
    let import_finished = status.has_finished();
    let github_repository = detect_github_source(project, &status).await;
    let collapse_issue_count = if !import_finished && !status.running && github_repository.is_none()
    {
        fetch_collapse_issue_count(&token, owner, name).await
    } else {
        None
    };
    let last_error = (!status.error.trim().is_empty()).then(|| status.error.trim().to_owned());
    Some(CollapseImportCache {
        collapse_repository: collapse_repository.to_owned(),
        checked_at_secs: now,
        import_finished,
        running: status.running,
        github_repository,
        collapse_issue_count,
        last_error,
    })
}

async fn detect_github_source(
    project: &ProjectItem,
    status: &velocity_collapse::GitHubIssueImportStatus,
) -> Option<String> {
    if let Some(saved) = project.github_remote.as_deref() {
        let trimmed = saved.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    if let Some(downstream) = project
        .collapse_mirror_health
        .as_ref()
        .and_then(|health| health.downstream_github.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some(downstream.to_owned());
    }
    for candidate in [
        &status.github_repository,
        &status.suggested_github_repository,
    ] {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    let path = std::path::PathBuf::from(&project.repository_path);
    tauri::async_runtime::spawn_blocking(move || {
        velocity_github::checkout_repository(&path).map(|name| name.full_name())
    })
    .await
    .ok()
    .flatten()
}

async fn fetch_collapse_issue_count(token: &str, owner: &str, name: &str) -> Option<u64> {
    let api = velocity_collapse::issues::CollapseIssueApi::new().ok()?;
    let velocity_collapse::issues::Conditional::Modified { value, .. } = api
        .list_issues(Some(token), owner, name, Some("all"), None, 1, 0, None)
        .await
        .ok()?
    else {
        return None;
    };
    let total = value.open_count.max(0) + value.closed_count.max(0);
    u64::try_from(total).ok()
}

fn collapse_issues_not_imported_item(
    cache: &CollapseImportCache,
    now_ms: u64,
) -> ProjectHealthItem {
    let action = match cache.last_error.as_deref() {
        Some(error) => format!("Last import attempt failed: {error}"),
        None => "Click Import now to import GitHub issues into Collapse so dispatch can see them"
            .to_owned(),
    };
    let detail = match cache.github_repository.as_deref() {
        Some(github) => Some(format!("{github} → {}", cache.collapse_repository)),
        None => Some(cache.collapse_repository.clone()),
    };
    ProjectHealthItem::new(
        COLLAPSE_ISSUES_NOT_IMPORTED_KEY,
        HealthSignalKind::CollapseIssuesNotImported,
        HealthSeverity::Warning,
        COLLAPSE_ISSUES_NOT_IMPORTED_SIGNAL,
        action,
        now_ms,
    )
    .with_detail(detail)
}

/// Signal 1: Mirror behind the base for more than 10 min (#509).
async fn check_mirror(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    if let Some(divergence) = &project.rosen_mirror_divergence {
        let detail = format!(
            "Base {} · mirror {}",
            short_commit(&divergence.base_commit),
            short_commit(&divergence.mirror_commit)
        );
        return Some(
            ProjectHealthItem::new(
                "mirror-behind",
                HealthSignalKind::MirrorBehind,
                HealthSeverity::Error,
                "Mirror diverged from Rosen base",
                "Showing mirror notice with both tips",
                now_ms,
            )
            .with_detail(Some(detail)),
        );
    }
    let lag = velocity_rosen::mirror_lag(server).await.ok().flatten()?;
    let waited = lag
        .behind_since_secs
        .map_or(0, |since| now.saturating_sub(since));
    if !lag.rejected && !lag.not_owner && waited < MIRROR_BEHIND_LIMIT_SECONDS {
        return None;
    }
    let tips = format!(
        "Base {} · mirror {}",
        short_commit(&lag.base_commit),
        lag.pushed_commit.as_deref().map_or("none", short_commit)
    );
    let signal = if waited >= MIRROR_BEHIND_LIMIT_SECONDS {
        format!("Mirror behind base for {}", format_duration(waited))
    } else {
        "Mirror behind Rosen base".to_owned()
    };
    if lag.not_owner {
        return Some(
            ProjectHealthItem::new(
                "mirror-behind",
                HealthSignalKind::MirrorBehind,
                HealthSeverity::Warning,
                signal,
                "This Rosen server is not the mirror owner",
                now_ms,
            )
            .with_detail(Some(tips)),
        );
    }
    let attempt = retry_mirror_if_due(application, project, server, &lag, now).await?;
    let detail = match &lag.error {
        Some(error) => Some(format!(
            "{tips} · {}",
            error.lines().next().unwrap_or(error)
        )),
        None => Some(tips),
    };
    Some(
        ProjectHealthItem::new(
            "mirror-behind",
            HealthSignalKind::MirrorBehind,
            attempt.severity,
            signal,
            attempt.action,
            attempt.at_secs.saturating_mul(1000),
        )
        .with_detail(detail),
    )
}

async fn retry_mirror_if_due(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    lag: &velocity_rosen::MirrorLag,
    now: u64,
) -> Option<MirrorAttempt> {
    let previous = with_tracker(application, project.id, |tracker| {
        tracker.mirror_attempt.clone()
    });
    if let Some(prev) = previous
        && prev.base_commit == lag.base_commit
        && prev.pushed_commit == lag.pushed_commit
        && now.saturating_sub(prev.at_secs) < MIRROR_RETRY_COOLDOWN_SECS
    {
        return Some(prev);
    }
    if crate::rosen::paused(application) {
        return Some(MirrorAttempt {
            base_commit: lag.base_commit.clone(),
            pushed_commit: lag.pushed_commit.clone(),
            at_secs: now,
            action: "Cannot retry push: Rosen servers are paused".into(),
            severity: HealthSeverity::Warning,
        });
    }
    let (action, severity, caught_up) =
        match crate::rosen_mirror::retry_for_health(application, project.id, server).await {
            Ok(MirrorRetryOutcome::UpToDate) => return None,
            Ok(MirrorRetryOutcome::RetriedPush) => (
                "Retried mirror push".to_owned(),
                HealthSeverity::Warning,
                None,
            ),
            Ok(MirrorRetryOutcome::CaughtUp { commits, new_base }) => (
                format!(
                    "Caught up {commits} commit(s) from mirror to {} (#501)",
                    short_commit(&new_base)
                ),
                HealthSeverity::Info,
                Some(new_base),
            ),
            Ok(MirrorRetryOutcome::Diverged {
                base_commit,
                mirror_commit,
            }) => (
                format!(
                    "Mirror diverged · showing both tips ({} vs {})",
                    short_commit(&base_commit),
                    short_commit(&mirror_commit)
                ),
                HealthSeverity::Error,
                None,
            ),
            Ok(MirrorRetryOutcome::NotApplicable(reason)) => {
                (reason, HealthSeverity::Warning, None)
            }
            Err(error) => (
                format!("Mirror push retry failed: {error}"),
                HealthSeverity::Error,
                None,
            ),
        };
    crate::diagnostics::record(SOURCE, &format!("{}: {action}", project.name));
    if caught_up.is_some() {
        record_recent_action(
            application,
            project.id,
            ProjectHealthItem::new(
                "mirror-caught-up",
                HealthSignalKind::MirrorBehind,
                HealthSeverity::Info,
                "Mirror was ahead of Rosen base",
                action,
                now.saturating_mul(1000),
            ),
            now,
        );
        return None;
    }
    let attempt = MirrorAttempt {
        base_commit: lag.base_commit.clone(),
        pushed_commit: lag.pushed_commit.clone(),
        at_secs: now,
        action,
        severity,
    };
    with_tracker(application, project.id, |tracker| {
        tracker.mirror_attempt = Some(attempt.clone());
    });
    Some(attempt)
}

/// Signal 2a: Landed changes have had no post-land verification result for > 1 h (#567).
async fn check_post_land_stalled(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    last_outcome: Option<&str>,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let reading = server.clone();
    let stall =
        tauri::async_runtime::spawn_blocking(move || integration::post_land_stall(&reading))
            .await
            .ok()?
            .ok()??;
    let waited = now.saturating_sub(stall.since);
    if stall.lands == 0 || waited < POST_LAND_STALL_LIMIT_SECONDS {
        return None;
    }
    let noun = if stall.lands == 1 {
        "change"
    } else {
        "changes"
    };
    let signal = format!(
        "Landed {} {noun} with no post-land result for > 1 h ({})",
        stall.lands,
        format_duration(waited)
    );
    let (action, severity) = start_or_explain_integration(application, project.id, server).await;
    let oldest = format!(
        "Oldest unverified commit: {}",
        short_commit(&stall.oldest_commit)
    );
    let detail = match last_outcome {
        Some(outcome) => format!("{oldest} · Last build: {outcome}"),
        None => oldest,
    };
    Some(
        ProjectHealthItem::new(
            "post-land-stalled",
            HealthSignalKind::PostLandStalled,
            severity,
            signal,
            action,
            now_ms,
        )
        .with_detail(Some(detail)),
    )
}

/// Signal 2: Integration build hasn't covered the base for N lands or H hours (#509).
async fn check_integration_uncovered(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    last_outcome: Option<&str>,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let reading = server.clone();
    let uncovered = tauri::async_runtime::spawn_blocking(move || integration::uncovered(&reading))
        .await
        .ok()?
        .ok()?;
    let waited = uncovered.since.map(|since| now.saturating_sub(since));
    let overdue = waited.is_some_and(|secs| secs >= INTEGRATION_UNCOVERED_SECONDS);
    if uncovered.lands == 0 || !(overdue || uncovered.lands >= INTEGRATION_UNCOVERED_LANDS) {
        return None;
    }
    let signal = match waited {
        Some(secs) => format!(
            "Integration build has not covered the base for {} ({} lands)",
            format_duration(secs),
            uncovered.lands
        ),
        None => format!(
            "Integration build has not covered the last {} lands",
            uncovered.lands
        ),
    };
    let (action, severity) = start_or_explain_integration(application, project.id, server).await;
    let detail = last_outcome.map(|outcome| format!("Last build: {outcome}"));
    Some(
        ProjectHealthItem::new(
            "integration-uncovered",
            HealthSignalKind::IntegrationUncovered,
            severity,
            signal,
            action,
            now_ms,
        )
        .with_detail(detail),
    )
}

async fn start_or_explain_integration(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> (String, HealthSeverity) {
    if crate::rosen_landing::is_verifying(application, project) {
        return ("Integration build is running".into(), HealthSeverity::Info);
    }
    if crate::rosen::paused(application) {
        return (
            "Cannot start integration build: Rosen servers are paused".into(),
            HealthSeverity::Warning,
        );
    }
    if velocity_rosen::memory::is_low() {
        return (
            "Cannot start integration build: machine memory is low".into(),
            HealthSeverity::Warning,
        );
    }
    match integration::try_slot() {
        Ok(Some(slot)) => drop(slot),
        Ok(None) => {
            return (
                "Cannot start integration build: another project holds the integration slot".into(),
                HealthSeverity::Warning,
            );
        }
        Err(error) => {
            return (
                format!("Cannot start integration build: slot lock error ({error})"),
                HealthSeverity::Error,
            );
        }
    }
    let batch = match post_land::next_batch(server).await {
        Ok(Some(batch)) => batch,
        Ok(None) => {
            return (
                "Cannot start integration build: waiting on an unfixed post-land failure or no pending batch".into(),
                HealthSeverity::Warning,
            );
        }
        Err(error) => {
            return (
                format!("Cannot start integration build: Rosen server unreachable ({error})"),
                HealthSeverity::Error,
            );
        }
    };
    let idle = crate::continuous_dispatch::integration_idle(application, project).unwrap_or(false);
    let reading = server.clone();
    let commit = batch.commit.clone();
    let due =
        tauri::async_runtime::spawn_blocking(move || integration::due(&reading, &commit, idle))
            .await
            .ok()
            .and_then(Result::ok);
    if due == Some(false) {
        return (
            format!(
                "Cannot start integration build: last build already failed at {} (waiting for a fix to land)",
                short_commit(&batch.commit)
            ),
            HealthSeverity::Warning,
        );
    }
    crate::rosen_landing::wake(application);
    (
        format!(
            "Started integration build on {}",
            short_commit(&batch.commit)
        ),
        HealthSeverity::Info,
    )
}

/// Signal 3: Integration or GitHub CI red on the base (#507, #509, #558).
async fn check_base_red(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    now_ms: u64,
) -> Vec<ProjectHealthItem> {
    let mut items = Vec::new();
    if let Some(ci) = &project.base_ci
        && ci.state == BaseCiState::Failed
    {
        let open = selected_ci_repair(application, project, server, ci.commit.as_deref()).await;
        let repair_thread = open
            .as_ref()
            .filter(|repair| repair.thread_state != RepairThreadState::Deleted)
            .map(|repair| repair.thread);
        let action = format_ci_repair_action_for_source(
            open.as_ref(),
            ci.commit.as_deref(),
            project.spec_locked,
            ci.source,
        );
        let commit = ci
            .commit
            .as_deref()
            .map(|id| format!(" at {}", short_commit(id)))
            .unwrap_or_default();
        let detail = (!ci.failing.is_empty()).then(|| ci.failing.join(" · "));
        let label = match ci.source {
            BaseCiSource::GitHub => "GitHub CI",
            BaseCiSource::Rosen => "Rosen post-land",
        };
        items.push(
            ProjectHealthItem::new(
                "base-red:ci",
                HealthSignalKind::BaseRed,
                HealthSeverity::Error,
                format!("{label} red on base{commit}"),
                action,
                now_ms,
            )
            .with_detail(detail)
            .with_thread(repair_thread),
        );
    }
    if let Some(item) = check_integration_red(application, project, server, now_ms).await {
        items.push(item);
    }
    items
}

async fn check_integration_red(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let failures = velocity_rosen::build_repairs::build_failures(server)
        .await
        .unwrap_or_default();
    let failure_commits: Vec<String> = failures
        .iter()
        .map(|failure| failure.commit.clone())
        .collect();
    let covered = velocity_rosen::build_repairs::repair_coverage(server, &failure_commits)
        .await
        .unwrap_or_default();
    let active_failures: Vec<_> = failures
        .into_iter()
        .filter(|failure| !failure.reverted && !covered.contains(&failure.commit))
        .collect();
    let open_repairs: Vec<_> = project
        .build_repairs
        .iter()
        .filter(|repair| !repair.closed)
        .collect();
    if active_failures.is_empty() && open_repairs.is_empty() {
        return None;
    }
    let repair_thread = open_repairs.first().map(|repair| repair.thread);
    let action = match repair_thread {
        Some(thread) => format!("Fix-forward thread #{thread} active"),
        None if !project.spec_locked => {
            "Cannot start fix-forward thread: project spec is not locked".into()
        }
        None => {
            crate::rosen_landing::wake(application);
            "Triggered fix-forward thread for red integration build".into()
        }
    };
    let detail = active_failures
        .first()
        .map(|failure| format!("Culprit {}", short_commit(&failure.commit)))
        .or_else(|| {
            open_repairs
                .first()
                .map(|repair| format!("Culprit {}", short_commit(&repair.commit)))
        });
    Some(
        ProjectHealthItem::new(
            "base-red:integration",
            HealthSignalKind::BaseRed,
            HealthSeverity::Error,
            "Integration build red on base",
            action,
            now_ms,
        )
        .with_detail(detail)
        .with_thread(repair_thread),
    )
}

async fn selected_ci_repair(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    failing_commit: Option<&str>,
) -> Option<OpenRepair> {
    let repair = project
        .ci_repairs
        .iter()
        .rev()
        .find(|candidate| {
            candidate.landed_commit.is_none()
                && project.threads.iter().any(|thread| {
                    thread.id == candidate.thread
                        && matches!(
                            thread.state,
                            ThreadState::Draft
                                | ThreadState::Spec
                                | ThreadState::Working
                                | ThreadState::Review
                        )
                })
        })
        .or_else(|| project.ci_repairs.last())?;
    let thread = project
        .threads
        .iter()
        .find(|candidate| candidate.id == repair.thread);
    let live = application
        .try_state::<crate::threads::ManagedThreads>()
        .is_some_and(|threads| {
            threads
                .lock()
                .is_ok_and(|threads| threads.is_live((project.id, repair.thread)))
        });
    let landed_commit =
        crate::base_ci::resolve_repair_landed_commit(application, Some(server), repair, thread)
            .await;
    let thread_state = crate::base_ci::repair_thread_state(thread, landed_commit.as_deref(), live);
    let commit_order = match (landed_commit.as_deref(), failing_commit) {
        (Some(landed), Some(failing)) => {
            crate::base_ci::resolve_commit_order(application, Some(server), landed, failing).await
        }
        _ => CommitOrder::Unknown,
    };
    Some(OpenRepair {
        workflow: repair.workflow.clone(),
        job: repair.job.clone(),
        commit: repair.commit.clone(),
        run: repair.run,
        thread: repair.thread,
        landed_commit,
        signature: repair.signature.clone(),
        thread_state,
        commit_order,
    })
}

/// Signal 4: Landed issue still open > H hours with no green integration (#504, #509).
fn check_landed_waiting(
    dispatch: &DispatchHealthSnapshot,
    last_outcome: Option<&str>,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let (first, rest) = dispatch.stalled_landed.split_first()?;
    let listed = std::iter::once(first)
        .chain(rest.iter().take(4))
        .map(|number| format!("#{number}"))
        .collect::<Vec<_>>()
        .join(", ");
    let more = if rest.len() > 4 { ", …" } else { "" };
    let count = dispatch.stalled_landed.len();
    let subject = if count == 1 {
        "1 landed issue waits".to_owned()
    } else {
        format!("{count} landed issues wait")
    };
    let signal = format!("{subject} for a green integration build ({listed}{more})");
    let reason = last_outcome.unwrap_or("no integration build has finished");
    Some(
        ProjectHealthItem::new(
            "landed-waiting",
            HealthSignalKind::LandedIssuesWaiting,
            HealthSeverity::Warning,
            signal,
            "Waiting for a green integration build to close landed issues",
            now_ms,
        )
        .with_detail(Some(format!("Last build: {reason}"))),
    )
}

/// Signal 5: Issue parked more than the set time (#506, #509).
fn collect_parked_items(application: &AppHandle, project: u32, now: u64) -> Vec<ProjectHealthItem> {
    with_tracker(application, project, |tracker| {
        tracker
            .parked
            .iter()
            .filter_map(|(&issue, record)| parked_item(issue, record, now))
            .collect()
    })
}

fn parked_item(issue: u32, record: &ParkedRecord, now: u64) -> Option<ProjectHealthItem> {
    match record {
        ParkedRecord::Resolved {
            action,
            acted_at_ms,
            expires_secs,
        } if *expires_secs > now => Some(ProjectHealthItem::new(
            format!("parked:{issue}"),
            HealthSignalKind::IssueParked,
            HealthSeverity::Info,
            format!("Issue #{issue} parked under no-change"),
            action.clone(),
            *acted_at_ms,
        )),
        ParkedRecord::Failed { error, acted_at_ms } => Some(
            ProjectHealthItem::new(
                format!("parked:{issue}"),
                HealthSignalKind::IssueParked,
                HealthSeverity::Warning,
                format!("Issue #{issue} parked under no-change"),
                "Could not resolve parked no-change issue",
                *acted_at_ms,
            )
            .with_detail(Some(error.clone())),
        ),
        ParkedRecord::Manual { seen_secs }
            if now.saturating_sub(*seen_secs) >= MANUAL_PARKED_LIMIT_SECS =>
        {
            Some(ProjectHealthItem::new(
                format!("parked:{issue}"),
                HealthSignalKind::IssueParked,
                HealthSeverity::Warning,
                format!(
                    "Issue #{issue} parked under no-change for {} (no bot report)",
                    format_duration(now.saturating_sub(*seen_secs))
                ),
                "Needs manual review to remove no-change",
                seen_secs.saturating_mul(1000),
            ))
        }
        _ => None,
    }
}

/// Signal 6: Thread in review more than 20 min without landing (#487, #509).
async fn check_review_stalled(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    now: u64,
    now_ms: u64,
) -> Vec<ProjectHealthItem> {
    let mut items = Vec::new();
    let branch = project.default_branch.trim();
    let base_branch = if branch.is_empty() { "master" } else { branch };
    let review_threads: Vec<_> = project
        .threads
        .iter()
        .filter(|thread| {
            thread.state == ThreadState::Review
                && thread.review_stage != Some(ReviewStage::Fixing)
                && thread.rosen_change.is_some()
        })
        .collect();
    let active_keys: Vec<(u32, u64)> = review_threads
        .iter()
        .map(|thread| (thread.id, thread.rosen_progress_generation))
        .collect();
    with_tracker(application, project.id, |tracker| {
        tracker
            .review_seen_secs
            .retain(|key, _| active_keys.contains(key));
        tracker
            .review_checked
            .retain(|key, _| active_keys.contains(key));
    });
    for thread in review_threads {
        let Some(change) = thread.rosen_change.as_deref() else {
            continue;
        };
        let key = (thread.id, thread.rosen_progress_generation);
        let since = review_since(application, project.id, server, change, key, now).await;
        let waited = now.saturating_sub(since);
        if waited < REVIEW_STUCK_LIMIT_SECONDS {
            continue;
        }
        let attempt = check_stalled_review_thread(
            application,
            project.id,
            server,
            thread,
            change,
            base_branch,
            now,
        )
        .await;
        items.push(
            ProjectHealthItem::new(
                format!("review-stalled:{}", thread.id),
                HealthSignalKind::ReviewStalled,
                attempt.severity,
                format!(
                    "Thread #{} in review for {} without landing",
                    thread.id,
                    format_duration(waited)
                ),
                attempt.action,
                attempt.at_secs.saturating_mul(1000).min(now_ms),
            )
            .with_detail(Some(thread.title.clone()))
            .with_thread(Some(thread.id)),
        );
    }
    items
}

async fn review_since(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    change: &str,
    key: (u32, u64),
    now: u64,
) -> u64 {
    if let Ok(Some(named_at)) = velocity_rosen::landing::named_since(server, change).await {
        return named_at;
    }
    with_tracker(application, project, |tracker| {
        *tracker.review_seen_secs.entry(key).or_insert(now)
    })
}

async fn check_stalled_review_thread(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    thread: &velocity_store::ThreadItem,
    change: &str,
    base_branch: &str,
    now: u64,
) -> ReviewAttempt {
    let key = (thread.id, thread.rosen_progress_generation);
    if let Some(prev) = with_tracker(application, project, |tracker| {
        tracker.review_checked.get(&key).cloned()
    }) && now.saturating_sub(prev.at_secs) < REVIEW_CHECK_COOLDOWN_SECS
    {
        return prev;
    }
    let attempt = match velocity_rosen::landing::land_check(server, change).await {
        Ok(LandCheckOutcome::AlreadyLanded { commit }) => {
            let occurred_at_ms = velocity_rosen::landing::landed_at_ms(server, change, &commit)
                .await
                .ok()
                .flatten();
            let _ = crate::rosen_events::record_landed(
                application,
                project,
                change,
                thread.rosen_progress_generation,
                Some(&commit),
                occurred_at_ms,
            );
            let action = format!(
                "Change already landed at {} · synced thread state",
                short_commit(&commit)
            );
            record_recent_review_action(application, project, thread, &action, now);
            ReviewAttempt {
                at_secs: now,
                action,
                severity: HealthSeverity::Info,
            }
        }
        Ok(LandCheckOutcome::Refused(refusal)) => {
            let (reason, instructions) = refusal_instructions(change, base_branch, refusal);
            let message = format!("Land refused · {reason}\n\n{instructions}");
            let sent = crate::rosen_events::record_land_refusal(
                application,
                project,
                change,
                thread.rosen_progress_generation,
                &message,
            );
            let action = match sent {
                Ok(_) => format!("Asked Rosen and sent land refusal ({reason}) to author (#487)"),
                Err(error) => format!("Land refused ({reason}) · could not deliver: {error}"),
            };
            crate::diagnostics::record(
                SOURCE,
                &format!("Project {project} thread #{}: {action}", thread.id),
            );
            record_recent_review_action(application, project, thread, &action, now);
            ReviewAttempt {
                at_secs: now,
                action,
                severity: HealthSeverity::Warning,
            }
        }
        Ok(LandCheckOutcome::Landable) => {
            crate::rosen_landing::wake(application);
            ReviewAttempt {
                at_secs: now,
                action: "Change is landable · retried landing".into(),
                severity: HealthSeverity::Info,
            }
        }
        Err(error) => ReviewAttempt {
            at_secs: now,
            action: format!("Could not ask Rosen for land refusal: {error}"),
            severity: HealthSeverity::Warning,
        },
    };
    with_tracker(application, project, |tracker| {
        tracker.review_checked.insert(key, attempt.clone());
    });
    attempt
}

fn record_recent_review_action(
    application: &AppHandle,
    project: u32,
    thread: &velocity_store::ThreadItem,
    action: &str,
    now: u64,
) {
    record_recent_action(
        application,
        project,
        ProjectHealthItem::new(
            format!("review-stalled:{}", thread.id),
            HealthSignalKind::ReviewStalled,
            HealthSeverity::Info,
            format!("Thread #{} was in review over 20 min", thread.id),
            action.to_owned(),
            now.saturating_mul(1000),
        )
        .with_detail(Some(thread.title.clone()))
        .with_thread(Some(thread.id)),
        now,
    );
}

/// Signal 7: Slots free and open non-tracking issues, with nothing dispatched (#509, #616, #617).
/// Logs to `velocity.log` after 10 min; raises the title-bar health alert, one-shot desktop
/// notification, and persisted health history entry once per idle spell after 30 min.
fn check_dispatch_idle(
    application: &AppHandle,
    project: &ProjectItem,
    dispatch: &DispatchHealthSnapshot,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let slots = project.continuous_dispatch.slots;
    let busy = busy_count(project);
    let has_free_slot = slots > 0 && !project.continuous_dispatch.paused && busy < slots;
    let groups = partition_skipped_issues(&dispatch.skipped);
    let has_non_tracking = !groups.other.is_empty()
        || !groups.needs_owner.is_empty()
        || !groups.blocked_upstream.is_empty();
    let has_candidates =
        dispatch.ready_count > 0 || has_non_tracking || dispatch.backlog_error.is_some();
    let detail = if has_free_slot && has_candidates {
        dispatch_idle_detail(project, dispatch)
    } else {
        String::new()
    };
    let persisted_idle_since_secs = project.continuous_dispatch.idle_since_secs;
    let persisted_alert_active = persisted_idle_since_secs.is_some_and(|since| {
        let entry_id = format!("dispatch-idle:{since}");
        project
            .health_history
            .iter()
            .any(|entry| entry.id == entry_id && entry.ended_at_ms.is_none())
    });
    let observation = with_tracker(application, project.id, |tracker| {
        tracker.dispatch_idle.observe(DispatchIdleSample {
            now_secs: now,
            has_free_slot,
            has_candidates,
            last_dispatched_secs: dispatch.last_dispatched_secs,
            persisted_idle_since_secs,
            persisted_alert_active,
            busy,
            slots,
            detail: &detail,
        })
    });
    if let Some(entry) = observation.history_update {
        persist_health_history(application, project.id, entry);
    }
    if observation.should_log {
        log_dispatch_idle(
            application,
            project,
            busy,
            slots,
            now,
            observation.active_alert,
            &detail,
        );
    }
    let active = observation.active_alert?;
    if observation.raise_notification {
        let (title, body) = dispatch_idle_notification_text(
            &project.name,
            busy,
            slots,
            active.idle_seconds,
            &dispatch.blocked_issues,
            &detail,
        );
        crate::desktop_notifications::send(title, body);
    }
    let upstream_only = dispatch.ready_count == 0
        && dispatch.backlog_error.is_none()
        && groups.other.is_empty()
        && groups.needs_owner.is_empty()
        && !groups.blocked_upstream.is_empty();
    let severity = if upstream_only {
        HealthSeverity::Info
    } else {
        HealthSeverity::Warning
    };
    Some(dispatch_idle_item(
        severity,
        busy,
        slots,
        active,
        dispatch_idle_action(&groups),
        detail,
        dispatch.blocked_issues.clone(),
        now_ms,
    ))
}

fn log_dispatch_idle(
    application: &AppHandle,
    project: &ProjectItem,
    busy: u32,
    slots: u32,
    now: u64,
    active_alert: Option<velocity_core::project_health::DispatchIdleActive>,
    detail: &str,
) {
    let waited = active_alert
        .map(|active| active.idle_seconds)
        .unwrap_or_else(|| {
            with_tracker(application, project.id, |tracker| {
                tracker
                    .dispatch_idle
                    .idle_since_secs
                    .map_or(0, |since| now.saturating_sub(since))
            })
        });
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "{}: slots free ({busy}/{slots}) for {} with nothing dispatched · {detail}",
            project.name,
            format_duration(waited)
        ),
    );
}

/// Immediately clears an active dispatch-idle alert and finalizes its persisted history entry
/// as soon as continuous or manual dispatch starts work on `project` (#616, #618).
pub(crate) fn notify_dispatched(application: &AppHandle, project: u32, now_secs: u64) {
    if let Some(store_state) = application.try_state::<crate::state::ManagedStateStore>()
        && let Ok(mut store) = crate::threads::lock(store_state.inner(), "persisted state")
    {
        let _ = store.set_dispatch_idle_since(project, None);
    }
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let (history_entry, updated_items) = {
        let mut reg = registry(&managed);
        let history_entry = reg
            .projects
            .entry(project)
            .or_default()
            .dispatch_idle
            .record_dispatched(now_secs);
        let list = reg.views.entry(project).or_default();
        let before = list.len();
        list.retain(|item| item.key != DISPATCH_IDLE_KEY);
        let updated = (list.len() != before || history_entry.is_some()).then(|| list.clone());
        (history_entry, updated)
    };
    if let Some(entry) = history_entry {
        persist_health_history(application, project, entry);
    }
    if let Some(items) = updated_items {
        emit_view(application, project, items);
    }
}

/// Rebuilds the active `DispatchIdle` item immediately after an owner unblock action (#616).
fn refresh_dispatch_idle_view(application: &AppHandle, project_id: u32) {
    let Some(project) = persisted_project(application, project_id) else {
        return;
    };
    let now_ms = unix_millis();
    let now = now_ms / 1000;
    let dispatch = crate::continuous_dispatch::health_snapshot(application, project_id, now)
        .unwrap_or_default();
    let next = check_dispatch_idle(application, &project, &dispatch, now, now_ms);
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let updated = {
        let mut reg = registry(&managed);
        let list = reg.views.entry(project_id).or_default();
        match next {
            Some(item) => {
                upsert_item(list, item);
                Some(list.clone())
            }
            None => {
                let before = list.len();
                list.retain(|item| item.key != DISPATCH_IDLE_KEY);
                (list.len() != before).then(|| list.clone())
            }
        }
    };
    if let Some(items) = updated {
        emit_view(application, project_id, items);
    }
}

fn persist_health_history(application: &AppHandle, project: u32, entry: ProjectHealthHistoryEntry) {
    let Some(store_state) = application.try_state::<crate::state::ManagedStateStore>() else {
        return;
    };
    if let Ok(mut store) = crate::threads::lock(store_state.inner(), "persisted state")
        && let Err(error) = store.record_project_health_history(project, entry)
    {
        crate::diagnostics::record(
            SOURCE,
            &format!("Project {project}: could not save health history: {error}"),
        );
    }
}

fn dispatch_idle_action(
    groups: &velocity_threads::continuous_dispatch::SkippedIssueGroups<'_>,
) -> String {
    let mut parts = Vec::new();
    if !groups.other.is_empty() {
        parts.push(format!("{} skipped", groups.other.len()));
    }
    if !groups.needs_owner.is_empty() {
        parts.push(format!("{} needs owner", groups.needs_owner.len()));
    }
    if !groups.blocked_upstream.is_empty() {
        parts.push(format!(
            "{} blocked upstream (checked daily)",
            groups.blocked_upstream.len()
        ));
    }
    if !groups.tracking.is_empty() {
        parts.push(format!("{} tracking", groups.tracking.len()));
    }
    if parts.is_empty() {
        "Logged why continuous dispatch is idle".into()
    } else {
        parts.join(" · ")
    }
}

fn dispatch_idle_detail(project: &ProjectItem, dispatch: &DispatchHealthSnapshot) -> String {
    let mut parts = Vec::new();
    if let Some(error) = &dispatch.backlog_error {
        parts.push(format!("backlog error: {error}"));
    }
    if !project.spec_locked {
        parts.push("spec is not locked".into());
    }
    if velocity_rosen::memory::is_low() {
        parts.push("machine memory is low".into());
    }
    if !dispatch.stale_released.is_empty() {
        let released: Vec<String> = dispatch
            .stale_released
            .iter()
            .take(6)
            .map(|(number, reason)| format!("#{number} ({reason})"))
            .collect();
        parts.push(released.join(", "));
    }
    if !dispatch.skipped.is_empty() {
        parts.push(summarize_skipped_issues(&dispatch.skipped, 6));
    } else if dispatch.ready_count > 0 && parts.is_empty() {
        parts.push(format!(
            "{} ready work item(s) waiting for dispatch pass",
            dispatch.ready_count
        ));
    } else if parts.is_empty() {
        parts.push(format!("{} open issue(s) inspected", dispatch.open_issues));
    }
    parts.join(" · ")
}

/// Signal 8: Post-land check stopped for running too long (#486, #509).
async fn check_post_land_stopped(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> Option<ProjectHealthItem> {
    let record = with_tracker(application, project, |tracker| {
        tracker.stopped_check.clone()
    })?;
    let reading = server.clone();
    let green = tauri::async_runtime::spawn_blocking(move || integration::last_green(&reading))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    if green.as_deref() == Some(record.commit.as_str()) {
        with_tracker(application, project, |tracker| {
            tracker.stopped_check = None;
        });
        return None;
    }
    Some(stopped_check_item(
        &record.commit,
        &record.command,
        &record.stopped,
        record.acted_at_ms,
    ))
}

fn stopped_check_item(
    commit: &str,
    command: &str,
    stopped: &StoppedCheck,
    acted_at_ms: u64,
) -> ProjectHealthItem {
    let detail = format_stopped_check_detail(
        command,
        stopped.test.as_deref(),
        stopped.process.as_deref(),
        &stopped.reason,
    );
    ProjectHealthItem::new(
        "post-land-stopped",
        HealthSignalKind::PostLandStopped,
        HealthSeverity::Error,
        format!("Post-land check stopped on {}", short_commit(commit)),
        "Stopped hung check and reported test and process (#486)",
        acted_at_ms,
    )
    .with_detail(Some(detail))
}

/// Signals 9 & 10: Updater manifest lag behind published releases, and refused release
/// gates lacking a green `velocity/integration` status (#521, #593).
async fn check_release_health(
    application: &AppHandle,
    project: &ProjectItem,
    server: &ProjectServer,
    now: u64,
    now_ms: u64,
) -> Vec<ProjectHealthItem> {
    let repository = project.github_remote.as_deref();
    if repository.is_none() && project.collapse_repository.is_none() {
        return Vec::new();
    }
    let mut items = Vec::new();
    if let Some(item) =
        check_pending_release(application, project, repository, server, now, now_ms).await
    {
        items.push(item);
    }
    let Some(cache) = read_release_health(application, project, repository, now).await else {
        return items;
    };
    if let Some(lag) = &cache.manifest_lag {
        items.push(manifest_lag_item(lag, now_ms));
    }
    if let (Some(repository), Some(gate)) = (repository, &cache.refused_gate)
        && let Some(item) =
            handle_refused_gate(application, project, repository, server, gate, now, now_ms).await
    {
        items.push(item);
    }
    items
}

async fn read_release_health(
    application: &AppHandle,
    project: &ProjectItem,
    repository: Option<&str>,
    now: u64,
) -> Option<ReleaseHealthCache> {
    let cached = with_tracker(application, project.id, |tracker| {
        tracker.release_health.clone()
    });
    if let Some(cache) = cached {
        let ttl = if cache.refused_gate.is_some() {
            RELEASE_GATE_POLL_SECS
        } else {
            RELEASE_HEALTH_COOLDOWN_SECS
        };
        if now.saturating_sub(cache.checked_at_secs) < ttl {
            return Some(cache);
        }
    }
    if let Some(collapse_repo) = project.collapse_repository.as_deref()
        && let Some((owner, name)) = velocity_core::collapse::parse_repository(collapse_repo)
    {
        let token = tauri::async_runtime::spawn_blocking(velocity_collapse::load_credentials)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten()
            .map(|credentials| credentials.mcp_token);
        if let Some(manifest_lag) =
            crate::updates::collapse_manifest_lag(token.as_deref(), &owner, &name).await
        {
            let fetched = ReleaseHealthCache {
                checked_at_secs: now,
                manifest_lag,
                refused_gate: None,
            };
            with_tracker(application, project.id, |tracker| {
                tracker.release_health = Some(fetched.clone());
            });
            return Some(fetched);
        }
    }
    if project.effective_issue_host() == velocity_core::collapse::IssueHost::Collapse {
        return None;
    }
    let repository = repository?;
    let directory = Path::new(&project.repository_path).to_path_buf();
    let repo = repository.to_owned();
    let fetched = tauri::async_runtime::spawn_blocking(move || {
        let client = IssueClient::for_repository(&directory, &repo).ok()?;
        let manifest_lag = client.manifest_lag().ok().flatten();
        let refused_gate = client.latest_refused_release_gate().ok().flatten();
        Some(ReleaseHealthCache {
            checked_at_secs: now,
            manifest_lag,
            refused_gate,
        })
    })
    .await
    .ok()
    .flatten()?;
    with_tracker(application, project.id, |tracker| {
        tracker.release_health = Some(fetched.clone());
    });
    Some(fetched)
}

fn manifest_lag_item(lag: &ManifestLag, now_ms: u64) -> ProjectHealthItem {
    let action = if lag.latest_signed {
        "Published release has not updated updater/latest.json; cut a signed release to offer it to installed copies"
    } else {
        "Newest published release is unsigned; installed copies stay on the manifest version until a signed release updates updater/latest.json"
    };
    let detail = format!(
        "updater/latest.json: {} · newest published release: {}",
        lag.manifest_version, lag.latest_tag
    );
    ProjectHealthItem::new(
        "updater-manifest-behind",
        HealthSignalKind::UpdaterManifestBehind,
        HealthSeverity::Warning,
        format!(
            "Updater manifest is behind ({} vs {})",
            lag.manifest_version, lag.latest_release
        ),
        action,
        now_ms,
    )
    .with_detail(Some(detail))
}

async fn check_pending_release(
    application: &AppHandle,
    project: &ProjectItem,
    repository: Option<&str>,
    server: &ProjectServer,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let pending = with_tracker(application, project.id, |tracker| {
        tracker.pending_release.clone()
    })?;
    if project.collapse_repository.is_some()
        || project.effective_issue_host() == velocity_core::collapse::IssueHost::Collapse
    {
        return check_pending_collapse_release(
            application,
            project,
            repository,
            server,
            &pending,
            now,
            now_ms,
        )
        .await;
    }
    let green = if let Some(repository) = repository {
        publish_and_find_green(project, repository, server, &pending.commit).await
    } else {
        None
    };
    if let Some(green) = green {
        return dispatch_pending_release(
            application,
            project,
            repository,
            &pending,
            &green,
            now,
            now_ms,
        )
        .await;
    }
    let (action, severity) =
        start_or_explain_release_integration(application, project.id, &pending.commit);
    Some(ProjectHealthItem::new(
        "release-gate-pending",
        HealthSignalKind::ReleaseGateRefused,
        severity,
        format!(
            "Release {} waits for green integration on {}",
            pending.tag,
            short_commit(&pending.commit)
        ),
        action,
        now_ms,
    ))
}

async fn check_pending_collapse_release(
    application: &AppHandle,
    project: &ProjectItem,
    repository: Option<&str>,
    server: &ProjectServer,
    pending: &PendingRelease,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    use velocity_rosen::release::{ReleaseGateCheck, check_release_gate};
    match check_release_gate(server, Some(&pending.commit)).await {
        Ok(ReleaseGateCheck::Ready(release)) => {
            dispatch_pending_release(
                application,
                project,
                repository,
                pending,
                &release.green,
                now,
                now_ms,
            )
            .await
        }
        Ok(ReleaseGateCheck::Waiting {
            platforms,
            local_needs_run,
            message,
            ..
        }) => {
            let (action, severity) = if local_needs_run {
                let (local_action, local_severity) =
                    start_or_explain_release_integration(application, project.id, &pending.commit);
                let action = if platforms.is_empty() {
                    local_action
                } else {
                    format!("{message} · {local_action}")
                };
                (action, local_severity)
            } else {
                let missing_checker = platforms.iter().any(|platform| !platform.checker_connected);
                let severity = if missing_checker {
                    HealthSeverity::Warning
                } else {
                    HealthSeverity::Info
                };
                (message, severity)
            };
            Some(ProjectHealthItem::new(
                "release-gate-pending",
                HealthSignalKind::ReleaseGateRefused,
                severity,
                format!(
                    "Release {} waits for Rosen post-land green on {}",
                    pending.tag,
                    short_commit(&pending.commit)
                ),
                action,
                now_ms,
            ))
        }
        Err(error) => Some(ProjectHealthItem::new(
            "release-gate-pending",
            HealthSignalKind::ReleaseGateRefused,
            HealthSeverity::Warning,
            format!(
                "Release {} waits for Rosen post-land green on {}",
                pending.tag,
                short_commit(&pending.commit)
            ),
            error.to_string(),
            now_ms,
        )),
    }
}

async fn dispatch_pending_release(
    application: &AppHandle,
    project: &ProjectItem,
    repository: Option<&str>,
    pending: &PendingRelease,
    green: &str,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let outcome = if project.collapse_repository.is_some() {
        crate::release::trigger_pending_collapse_release(
            application,
            project.id,
            pending.tag.clone(),
            pending.commit.clone(),
            green.to_owned(),
        )
        .await
    } else if let Some(repository) = repository {
        dispatch_release_blocking(project, repository, &pending.tag, &pending.commit, green)
            .await
            .map(|()| {
                format!(
                    "Dispatched release {} at {} after green integration {}",
                    pending.tag,
                    short_commit(&pending.commit),
                    short_commit(green)
                )
            })
    } else {
        Err("This project has no Collapse or GitHub repository for releases".into())
    };
    match outcome {
        Ok(action) => {
            with_tracker(application, project.id, |tracker| {
                tracker.pending_release = None;
                tracker.release_health = None;
            });
            crate::diagnostics::record(SOURCE, &format!("{}: {action}", project.name));
            record_recent_action(
                application,
                project.id,
                ProjectHealthItem::new(
                    "release-gate-dispatched",
                    HealthSignalKind::ReleaseGateRefused,
                    HealthSeverity::Info,
                    format!("Queued release {} ready", pending.tag),
                    action,
                    now_ms,
                ),
                now,
            );
            None
        }
        Err(error) => Some(ProjectHealthItem::new(
            "release-gate-pending",
            HealthSignalKind::ReleaseGateRefused,
            HealthSeverity::Error,
            format!("Queued release {} failed to dispatch", pending.tag),
            error,
            now_ms,
        )),
    }
}

async fn handle_refused_gate(
    application: &AppHandle,
    project: &ProjectItem,
    repository: &str,
    server: &ProjectServer,
    gate: &RefusedReleaseGate,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let already_retried = project.retried_release_runs.contains(&gate.run_id)
        || with_tracker(application, project.id, |tracker| {
            tracker.retried_release_runs.contains(&gate.run_id)
        });
    if already_retried {
        return None;
    }
    let label = gate
        .tag
        .as_deref()
        .map(|tag| format!(" for {tag}"))
        .unwrap_or_default();
    if let Some(green) = publish_and_find_green(project, repository, server, &gate.commit).await {
        return recover_refused_gate(
            application,
            project,
            repository,
            gate,
            &label,
            &green,
            now,
            now_ms,
        )
        .await;
    }
    let (action, severity) =
        start_or_explain_release_integration(application, project.id, &gate.commit);
    Some(
        ProjectHealthItem::new(
            "release-gate-refused",
            HealthSignalKind::ReleaseGateRefused,
            severity,
            format!(
                "Release gate refused{label} ({} has no green integration build)",
                short_commit(&gate.commit)
            ),
            action,
            now_ms,
        )
        .with_detail((!gate.url.is_empty()).then(|| gate.url.clone())),
    )
}

#[allow(clippy::too_many_arguments)]
async fn recover_refused_gate(
    application: &AppHandle,
    project: &ProjectItem,
    repository: &str,
    gate: &RefusedReleaseGate,
    label: &str,
    green: &str,
    now: u64,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let existing_tags =
        match load_existing_tags_for_recovery(project, repository, gate, green).await {
            Ok(tags) => tags,
            Err(error) => {
                return Some(refused_gate_error_item(
                    gate,
                    label,
                    format!("Could not read existing release tags: {error}"),
                    now_ms,
                ));
            }
        };
    let plan = plan_refused_gate_recovery(gate.tag.as_deref(), &gate.commit, green, &existing_tags);
    let action = match execute_gate_recovery(project, repository, gate, green, plan).await {
        Ok(action) => action,
        Err(error) => return Some(refused_gate_error_item(gate, label, error, now_ms)),
    };
    record_retried_release_run(application, project, gate.run_id).await;
    crate::diagnostics::record(SOURCE, &format!("{}: {action}", project.name));
    record_recent_action(
        application,
        project.id,
        ProjectHealthItem::new(
            "release-gate-recovered",
            HealthSignalKind::ReleaseGateRefused,
            HealthSeverity::Info,
            format!("Release gate satisfied{label}"),
            action,
            now_ms,
        ),
        now,
    );
    None
}

async fn load_existing_tags_for_recovery(
    project: &ProjectItem,
    repository: &str,
    gate: &RefusedReleaseGate,
    green: &str,
) -> Result<Vec<String>, String> {
    let needs_tags = gate
        .tag
        .as_deref()
        .is_some_and(|tag| !green.eq_ignore_ascii_case(&gate.commit) && is_beta_release_tag(tag));
    if !needs_tags {
        return Ok(Vec::new());
    }
    read_release_tags_blocking(project, repository).await
}

async fn execute_gate_recovery(
    project: &ProjectItem,
    repository: &str,
    gate: &RefusedReleaseGate,
    green: &str,
    plan: RefusedGateRecovery,
) -> Result<String, String> {
    match plan {
        RefusedGateRecovery::RedispatchRefusedTag { tag } => {
            dispatch_release_blocking(project, repository, &tag, &gate.commit, green)
                .await
                .map_err(|error| format!("Could not re-dispatch release.yml: {error}"))?;
            Ok(format!(
                "Integration passed at {}; re-dispatched release.yml for {tag}",
                short_commit(green)
            ))
        }
        RefusedGateRecovery::ReleaseGreenBeta { tag } => {
            dispatch_release_blocking(project, repository, &tag, green, green)
                .await
                .map_err(|error| format!("Could not dispatch release.yml for {tag}: {error}"))?;
            let refused = gate.tag.as_deref().unwrap_or("refused tag");
            Ok(format!(
                "Integration passed at {}; dispatched release.yml for {tag} instead of retrying {refused} on {}",
                short_commit(green),
                short_commit(&gate.commit)
            ))
        }
        RefusedGateRecovery::ReportReady => Ok(match gate.tag.as_deref() {
            Some(tag) => format!(
                "Integration passed at {}; release can now be cut (left {tag} on {} alone)",
                short_commit(green),
                short_commit(&gate.commit)
            ),
            None => format!(
                "Integration passed at {}; release can now be cut",
                short_commit(green)
            ),
        }),
    }
}

fn refused_gate_error_item(
    gate: &RefusedReleaseGate,
    label: &str,
    action: String,
    now_ms: u64,
) -> ProjectHealthItem {
    ProjectHealthItem::new(
        "release-gate-refused",
        HealthSignalKind::ReleaseGateRefused,
        HealthSeverity::Error,
        format!("Release gate refused{label}"),
        action,
        now_ms,
    )
    .with_detail((!gate.url.is_empty()).then(|| gate.url.clone()))
}

async fn record_retried_release_run(application: &AppHandle, project: &ProjectItem, run_id: u64) {
    with_tracker(application, project.id, |tracker| {
        tracker.retried_release_runs.insert(run_id);
        tracker.release_health = None;
    });
    let project_id = project.id;
    let project_name = project.name.clone();
    let result = crate::state::with_store(application, move |store| {
        store
            .record_retried_release_run(project_id, run_id)
            .map_err(|error| error.to_string())
    })
    .await;
    if let Ok(Err(error)) = result {
        crate::diagnostics::record(
            SOURCE,
            &format!("{project_name}: could not persist handled release run {run_id}: {error}"),
        );
    }
}

async fn read_release_tags_blocking(
    project: &ProjectItem,
    repository: &str,
) -> Result<Vec<String>, String> {
    let directory = Path::new(&project.repository_path).to_path_buf();
    let repo = repository.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let client =
            IssueClient::for_repository(&directory, &repo).map_err(|error| error.to_string())?;
        client.release_tags().map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn publish_and_find_green(
    project: &ProjectItem,
    repository: &str,
    server: &ProjectServer,
    commit: &str,
) -> Option<String> {
    let reading = server.clone();
    let local_green =
        tauri::async_runtime::spawn_blocking(move || integration::last_green(&reading))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
    let directory = Path::new(&project.repository_path).to_path_buf();
    let repo = repository.to_owned();
    let target = commit.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let client = IssueClient::for_repository(&directory, &repo).ok()?;
        if let Some(green) = local_green.as_deref() {
            let _ = client.publish_integration_success(green);
        }
        client
            .integration_status_covering(&target)
            .ok()
            .flatten()
            .map(|coverage| coverage.commit)
    })
    .await
    .ok()
    .flatten()
}

async fn dispatch_release_blocking(
    project: &ProjectItem,
    repository: &str,
    tag: &str,
    commit: &str,
    green: &str,
) -> Result<(), String> {
    let directory = Path::new(&project.repository_path).to_path_buf();
    let (repo, tag, commit, green) = (
        repository.to_owned(),
        tag.to_owned(),
        commit.to_owned(),
        green.to_owned(),
    );
    tauri::async_runtime::spawn_blocking(move || {
        let client =
            IssueClient::for_repository(&directory, &repo).map_err(|error| error.to_string())?;
        client
            .dispatch_release(&tag, &commit, &green)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn start_or_explain_release_integration(
    application: &AppHandle,
    project: u32,
    commit: &str,
) -> (String, HealthSeverity) {
    if crate::rosen_landing::is_verifying(application, project) {
        return (
            format!(
                "Integration build is running to cover {}",
                short_commit(commit)
            ),
            HealthSeverity::Info,
        );
    }
    if crate::rosen::paused(application) {
        return (
            "Cannot start integration build for release gate: Rosen servers are paused".into(),
            HealthSeverity::Warning,
        );
    }
    if velocity_rosen::memory::is_low() {
        return (
            "Cannot start integration build for release gate: machine memory is low".into(),
            HealthSeverity::Warning,
        );
    }
    crate::rosen_landing::request_integration(application, project, Some(commit.to_owned()));
    (
        format!(
            "Started integration build for {} to satisfy the release gate",
            short_commit(commit)
        ),
        HealthSeverity::Info,
    )
}

async fn read_last_outcome(server: &ProjectServer) -> Option<String> {
    let reading = server.clone();
    tauri::async_runtime::spawn_blocking(move || integration::last_outcome(&reading))
        .await
        .ok()?
        .ok()
        .flatten()
}

fn record_recent_action(application: &AppHandle, project: u32, item: ProjectHealthItem, now: u64) {
    with_tracker(application, project, |tracker| {
        tracker.recent_actions.insert(
            item.key.clone(),
            RecentAction {
                item,
                expires_secs: now.saturating_add(RECENT_ACTION_TTL_SECS),
            },
        );
    });
}

fn collect_recent_actions(
    application: &AppHandle,
    project: u32,
    now: u64,
    active: &[ProjectHealthItem],
) -> Vec<ProjectHealthItem> {
    with_tracker(application, project, |tracker| {
        tracker
            .recent_actions
            .retain(|_, entry| entry.expires_secs > now);
        tracker
            .recent_actions
            .values()
            .filter(|entry| !active.iter().any(|item| item.key == entry.item.key))
            .map(|entry| entry.item.clone())
            .collect()
    })
}

/// Preserves each signal's `acted_at_ms` across periodic polls while its action and detail
/// are unchanged, so the strip shows when the action was actually taken rather than "now".
fn stabilize_and_publish(
    application: &AppHandle,
    project: u32,
    mut items: Vec<ProjectHealthItem>,
    now_ms: u64,
) {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return;
    };
    let changed = {
        let mut reg = registry(&managed);
        let tracker = reg.projects.entry(project).or_default();
        let active_keys: Vec<String> = items.iter().map(|item| item.key.clone()).collect();
        tracker
            .acted_times
            .retain(|key, _| active_keys.contains(key));
        for item in &mut items {
            let detail = item.detail.clone().unwrap_or_default();
            let stamped = item.acted_at_ms.parse::<u64>().unwrap_or(now_ms);
            match tracker.acted_times.get(&item.key) {
                Some((prev_action, prev_detail, prev_at))
                    if prev_action == &item.action && prev_detail == &detail =>
                {
                    item.acted_at_ms = prev_at.to_string();
                }
                _ => {
                    tracker
                        .acted_times
                        .insert(item.key.clone(), (item.action.clone(), detail, stamped));
                }
            }
        }
        let previous = reg.views.get(&project);
        if previous == Some(&items) {
            false
        } else {
            reg.views.insert(project, items.clone());
            true
        }
    };
    if changed {
        emit_view(application, project, items);
    }
}

fn upsert_item(list: &mut Vec<ProjectHealthItem>, item: ProjectHealthItem) {
    if let Some(existing) = list.iter_mut().find(|entry| entry.key == item.key) {
        *existing = item;
    } else {
        list.push(item);
    }
}

fn emit_view(application: &AppHandle, project: u32, items: Vec<ProjectHealthItem>) {
    let history = persisted_project(application, project)
        .map(|item| item.health_history)
        .unwrap_or_default();
    let view = ProjectHealthView {
        project_identifier: project,
        items,
        history,
    };
    if let Err(error) = (ProjectHealthChanged { view }).emit(application) {
        eprintln!("Could not publish project health for project {project}: {error}");
    }
}

fn with_tracker<T: Default>(
    application: &AppHandle,
    project: u32,
    action: impl FnOnce(&mut ProjectHealthTracker) -> T,
) -> T {
    let Some(managed) = application.try_state::<ManagedProjectHealth>() else {
        return T::default();
    };
    let mut reg = registry(&managed);
    action(reg.projects.entry(project).or_default())
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
