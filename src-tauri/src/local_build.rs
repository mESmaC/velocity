//! Velocity on its own landed fixes (SPEC.md F1 "Local builds", #512, #551). When the
//! project that is Velocity's own repository has caught up with its mirror and has a new
//! green integration commit that strictly descends from the running build's commit, a
//! release of that commit is built in the background, and the title bar offers a restart
//! into it. `velocity-updates` holds the builds and what is offered, `velocity-threads`
//! runs the build, and the restart takes the update's path, so state is kept and threads
//! resume. Nothing here is published: releases stay on CI, which alone holds the updater
//! key.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;
use velocity_rosen::ProjectServer;
use velocity_rosen::post_land::{CommitAncestry, commit_contains, compare_commits};
use velocity_store::duty::LandingProject;
use velocity_threads::local_release::{self, LocalRelease};
use velocity_updates::local_build::{
    CommitRelation, FailedAutoRestart, FailedBuild, IdleBlockers, InstallDecision, LocalBuild,
    LocalBuildPhase, LocalBuildSettings, LocalBuildState, LocalBuildView, LocalBuilds,
    POST_RESTART_HEALTH_WINDOW, PendingRestart, short_commit,
};

use crate::rosen_mirror::CatchUpCheck;
use crate::state::ManagedStateStore;

/// Commit embedded at compile time by `src-tauri/build.rs` (empty when unknown).
const BUILD_COMMIT: &str = env!("VELOCITY_BUILD_COMMIT");

/// The first look waits until startup work and resumed threads settle.
const FIRST_LOOK: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_secs(60);
/// Refresh existing local-build ancestry shortly after startup so ready builds and
/// waiting issues show up quickly without waiting 90 seconds (#601).
const INITIAL_RELATION_REFRESH: Duration = Duration::from_secs(15);
/// How often waiting-issue auto-restart readiness is sampled (#601).
const AUTO_RESTART_POLL: Duration = Duration::from_secs(10);
/// How often the post-restart startup and renderer heartbeat monitor samples health (#609).
const POST_RESTART_HEALTH_POLL: Duration = Duration::from_secs(5);
/// Grace period for the main window to open after a local-build restart (#609).
const STARTUP_WINDOW_GRACE: Duration = Duration::from_secs(45);
/// What a build needs free before it starts: two compiler jobs at about 1.5 GB each
/// (`scripts/release-local.ps1`) and the link.
const BUILD_MEMORY: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum AutoRestartScope {
    WaitingOnly,
    All,
}

/// One open issue waiting for Velocity to restart into a build containing its blocker's
/// agent-tooling land (#601).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WaitingIssueNeed {
    pub issue: u32,
    pub commit: String,
    pub required_commits: Vec<String>,
}

#[derive(Default)]
struct AutoRestartTracker {
    waiting: Vec<WaitingIssueNeed>,
    failed_auto: Option<FailedAutoRestart>,
}

/// Active local-build restart failure while Velocity issues are waiting for restart (#605).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestartFailureHealth {
    pub commit: Option<String>,
    pub reason: String,
    pub waiting_issues: u32,
    pub waiting_detail: String,
    pub auto_restart: bool,
}

pub struct ManagedLocalBuilds {
    state: Mutex<LocalBuildState>,
    auto: Mutex<AutoRestartTracker>,
    builds: LocalBuilds,
    app_data: PathBuf,
}

#[derive(Clone, serde::Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "local-build://status")]
pub struct LocalBuildStatusChanged {
    pub view: LocalBuildView,
}

fn state(local: &ManagedLocalBuilds) -> MutexGuard<'_, LocalBuildState> {
    local
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn auto_tracker(local: &ManagedLocalBuilds) -> MutexGuard<'_, AutoRestartTracker> {
    local
        .auto
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn initialize(application: &AppHandle) {
    let Ok(app_data) = application.path().app_data_dir() else {
        return;
    };
    let builds = LocalBuilds::new(&app_data);
    let version = application.package_info().version.to_string();
    let build_commit = (!BUILD_COMMIT.is_empty()).then_some(BUILD_COMMIT);
    let settings = builds.settings();
    let mut known = LocalBuildState::new(&version, build_commit, builds.list(), builds.failure());
    known.set_auto_restart(settings.auto_restart);
    let pending_to_verify = match builds.pending_restart() {
        Some(pending) if known.runs(&pending.commit) && !pending.is_stale(unix_seconds()) => {
            Some(pending)
        }
        Some(_) => {
            builds.clear_pending_restart();
            None
        }
        None => None,
    };
    application.manage(ManagedLocalBuilds {
        state: Mutex::new(known),
        auto: Mutex::new(AutoRestartTracker::default()),
        builds,
        app_data,
    });
    // A development build is not the copy the owner runs: it builds and replaces nothing.
    if cfg!(debug_assertions) || !cfg!(windows) {
        return;
    }
    if let Some(pending) = pending_to_verify {
        let monitor_handle = application.clone();
        tauri::async_runtime::spawn(async move {
            monitor_post_restart_health(monitor_handle, pending).await;
        });
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(INITIAL_RELATION_REFRESH).await;
        refresh_existing_relations(&handle).await;
        check_auto_restart(&handle, AutoRestartScope::WaitingOnly).await;
        let mut next_look = Instant::now() + FIRST_LOOK.saturating_sub(INITIAL_RELATION_REFRESH);
        loop {
            tokio::time::sleep(AUTO_RESTART_POLL).await;
            if Instant::now() >= next_look {
                look(&handle).await;
                check_auto_restart(&handle, AutoRestartScope::All).await;
                next_look = Instant::now() + LOOK_INTERVAL;
            } else {
                check_auto_restart(&handle, AutoRestartScope::WaitingOnly).await;
            }
        }
    });
}

/// The running release build's commit, or `None` on development/non-Windows copies that
/// never build or restart into local builds (#601).
pub(crate) fn running_commit(application: &AppHandle) -> Option<String> {
    if cfg!(debug_assertions) || !cfg!(windows) {
        return None;
    }
    application
        .try_state::<ManagedLocalBuilds>()
        .and_then(|local| state(&local).running_commit().map(str::to_owned))
}

/// Updates the count and detail of issues waiting for a restart into a build with new
/// agent tooling, and triggers an auto-restart check when waiting issues arrive (#601).
pub(crate) fn update_waiting_issues(application: &AppHandle, waiting: Vec<WaitingIssueNeed>) {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let changed = {
        let mut tracker = auto_tracker(&local);
        if tracker.waiting == waiting {
            false
        } else {
            tracker.waiting = waiting.clone();
            true
        }
    };
    let count = u32::try_from(waiting.len()).unwrap_or(u32::MAX);
    let detail = waiting_tooltip(&waiting);
    change(application, |state| state.set_waiting_issues(count, detail));
    crate::project_health::sync_local_build_restart_health(application);
    if changed && !waiting.is_empty() && !cfg!(debug_assertions) && cfg!(windows) {
        let handle = application.clone();
        tauri::async_runtime::spawn(async move {
            check_auto_restart(&handle, AutoRestartScope::WaitingOnly).await;
        });
    }
}

/// Returns the active restart failure when at least one Velocity issue is waiting for a
/// restart, so project health can surface why dispatch is blocked (#605).
pub(crate) fn restart_failure_health(application: &AppHandle) -> Option<RestartFailureHealth> {
    let local = application.try_state::<ManagedLocalBuilds>()?;
    let (waiting_issues, waiting_detail, auto_restart, phase_failure, fallback_commit) = {
        let current = state(&local);
        let view = current.view();
        let phase_failure = match &current.phase {
            LocalBuildPhase::RestartFailed(reason) => Some(reason.clone()),
            _ => None,
        };
        (
            view.waiting_issues,
            view.waiting_detail,
            view.auto_restart,
            phase_failure,
            current.newest_descendant_commit().map(str::to_owned),
        )
    };
    if waiting_issues == 0 {
        return None;
    }
    let failed_auto = auto_tracker(&local).failed_auto.clone();
    let (commit, reason) = match (phase_failure, failed_auto) {
        (Some(reason), Some(auto)) => (Some(auto.commit), reason),
        (Some(reason), None) => (fallback_commit, reason),
        (None, Some(auto)) => (Some(auto.commit), auto.reason),
        (None, None) => return None,
    };
    Some(RestartFailureHealth {
        commit,
        reason,
        waiting_issues,
        waiting_detail,
        auto_restart,
    })
}

fn waiting_tooltip(waiting: &[WaitingIssueNeed]) -> String {
    waiting
        .iter()
        .take(5)
        .map(|need| format!("#{} (needs {})", need.issue, short_commit(&need.commit)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[tauri::command]
#[specta::specta]
pub fn get_local_build_status(application: AppHandle) -> LocalBuildView {
    view(&application)
}

/// Enables or disables automatic restarts into the newest local build (default on) (#601).
#[tauri::command]
#[specta::specta]
pub fn set_local_build_auto_restart(
    application: AppHandle,
    enabled: bool,
) -> Result<LocalBuildView, String> {
    let local = application
        .try_state::<ManagedLocalBuilds>()
        .ok_or("Local builds haven't started yet")?;
    local
        .builds
        .save_settings(&LocalBuildSettings {
            auto_restart: enabled,
        })
        .map_err(|error| error.to_string())?;
    change(&application, |state| state.set_auto_restart(enabled));
    if enabled {
        auto_tracker(&local).failed_auto = None;
        if !cfg!(debug_assertions) && cfg!(windows) {
            let handle = application.clone();
            tauri::async_runtime::spawn(async move {
                check_auto_restart(&handle, AutoRestartScope::All).await;
            });
        }
    }
    crate::project_health::sync_local_build_restart_health(&application);
    Ok(view(&application))
}

/// Restarts Velocity into the kept local build of `commit` (#512, #551). Never installs an
/// ancestor on a forward restart; asks first when the running build's commit or ancestry
/// is unknown unless `confirmed` is true. Explicit rollback (`rollback = true`) is only
/// permitted for the kept previous build while running the newest local build.
#[tauri::command]
#[specta::specta]
pub async fn restart_into_local_build(
    application: AppHandle,
    commit: String,
    confirmed: bool,
    rollback: bool,
) -> LocalBuildView {
    let _ = perform_restart(&application, &commit, confirmed, rollback).await;
    view(&application)
}

enum BeginRestart {
    Proceed,
    Busy,
    Confirm(String),
    Refuse(String),
}

async fn perform_restart(
    application: &AppHandle,
    commit: &str,
    confirmed: bool,
    rollback: bool,
) -> Result<(), String> {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        let message = "Local builds haven't started yet".to_owned();
        crate::diagnostics::record(
            "local build",
            &format!("Restart into {} failed: {message}", short_commit(commit)),
        );
        return Err(message);
    };
    let Some(build) = target_build(&local, commit) else {
        let message = format!(
            "Local build {} is no longer available",
            short_commit(commit)
        );
        crate::diagnostics::record(
            "local build",
            &format!("Restart into {} failed: {message}", short_commit(commit)),
        );
        return Err(message);
    };
    let relation = live_relation(application, &local, &build.commit).await;
    match begin_restart(application, &build, relation, confirmed, rollback) {
        BeginRestart::Proceed => {}
        BeginRestart::Busy => {
            let message = "Another local build or restart is already in progress".to_owned();
            crate::diagnostics::record(
                "local build",
                &format!(
                    "Restart into {} skipped: {message}",
                    short_commit(&build.commit)
                ),
            );
            return Err(message);
        }
        BeginRestart::Confirm(message) | BeginRestart::Refuse(message) => {
            crate::project_health::sync_local_build_restart_health(application);
            return Err(message);
        }
    }
    if let Err(message) = restart(application, &build, rollback).await {
        crate::diagnostics::record(
            "local build",
            &format!(
                "RestartFailed for {}: {message}",
                short_commit(&build.commit)
            ),
        );
        change(application, |state| {
            state.phase = LocalBuildPhase::RestartFailed(message.clone());
        });
        crate::project_health::sync_local_build_restart_health(application);
        return Err(message);
    }
    Ok(())
}

/// Dismisses a pending unknown-commit confirmation prompt without restarting (#551).
#[tauri::command]
#[specta::specta]
pub fn cancel_local_build_restart(application: AppHandle) -> LocalBuildView {
    change(&application, |state| {
        if matches!(state.phase, LocalBuildPhase::ConfirmRestart(_)) {
            state.phase = LocalBuildPhase::Idle;
        }
    });
    view(&application)
}

/// Catches the Velocity project up from its mirror, refreshes kept-build ancestry, and
/// builds the project's last green commit only when it strictly descends from the running
/// build's commit (#551). Collapse mirrors skip the idle 60-second probe and catch up right
/// before starting a build so idle checks never keep Collapse awake (#572).
async fn look(application: &AppHandle) {
    if crate::rosen::paused(application) {
        return;
    }
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let Some(project) = velocity_project(application) else {
        return;
    };
    let collapse = project.project.collapse_repository.is_some();
    if !collapse
        && crate::rosen_mirror::ensure_caught_up(application, project.project.id).await
            != CatchUpCheck::Current
    {
        clear_waiting_for_memory(application);
        return;
    }
    let Ok(server) = crate::rosen::observation_server(application, project.project.id) else {
        return;
    };
    refresh_relations(application, &local, &server).await;
    let Some((wanted, headroom)) = observe_candidate(&local, &server).await else {
        return;
    };
    match wanted {
        Some(commit) if headroom => {
            if collapse {
                let Some(commit) =
                    confirm_collapse_candidate(application, &local, &project, &server).await
                else {
                    return;
                };
                build(application, &local, &project, &server, &commit).await;
            } else {
                build(application, &local, &project, &server, &commit).await;
            }
        }
        Some(commit) => change(application, |state| {
            state.phase = LocalBuildPhase::WaitingForMemory(commit);
        }),
        None => clear_waiting_for_memory(application),
    }
}

async fn observe_candidate(
    local: &ManagedLocalBuilds,
    server: &ProjectServer,
) -> Option<(Option<String>, bool)> {
    let green_server = server.clone();
    let observed = tauri::async_runtime::spawn_blocking(move || {
        let green = velocity_rosen::integration::last_green(&green_server);
        (green, velocity_rosen::memory::has_headroom(BUILD_MEMORY))
    })
    .await;
    let Ok((Ok(green), headroom)) = observed else {
        return None;
    };
    let relation = match green.as_deref() {
        Some(commit) => commit_relation(local, server, commit).await,
        None => CommitRelation::Unknown,
    };
    let wanted = state(local).wanted(green.as_deref(), relation);
    Some((wanted, headroom))
}

async fn confirm_collapse_candidate(
    application: &AppHandle,
    local: &ManagedLocalBuilds,
    project: &LandingProject,
    server: &ProjectServer,
) -> Option<String> {
    if crate::rosen_mirror::ensure_caught_up(application, project.project.id).await
        != CatchUpCheck::Current
    {
        clear_waiting_for_memory(application);
        return None;
    }
    refresh_relations(application, local, server).await;
    let (wanted, headroom) = observe_candidate(local, server).await?;
    match wanted {
        Some(commit) if headroom => Some(commit),
        Some(commit) => {
            change(application, |state| {
                state.phase = LocalBuildPhase::WaitingForMemory(commit);
            });
            None
        }
        None => {
            clear_waiting_for_memory(application);
            None
        }
    }
}

fn clear_waiting_for_memory(application: &AppHandle) {
    change(application, |state| {
        if matches!(state.phase, LocalBuildPhase::WaitingForMemory(_)) {
            state.phase = LocalBuildPhase::Idle;
        }
    });
}

async fn refresh_relations(
    application: &AppHandle,
    local: &ManagedLocalBuilds,
    server: &ProjectServer,
) {
    let builds = state(local).builds().to_vec();
    let relations = compute_relations(local, server, &builds).await;
    change(application, |state| state.set_relations(relations));
}

async fn compute_relations(
    local: &ManagedLocalBuilds,
    server: &ProjectServer,
    builds: &[LocalBuild],
) -> BTreeMap<String, CommitRelation> {
    let mut relations = BTreeMap::new();
    for build in builds {
        let relation = commit_relation(local, server, &build.commit).await;
        relations.insert(build.commit.clone(), relation);
    }
    relations
}

async fn commit_relation(
    local: &ManagedLocalBuilds,
    server: &ProjectServer,
    candidate: &str,
) -> CommitRelation {
    let running = state(local).running_commit().map(str::to_owned);
    let Some(running) = running else {
        return CommitRelation::Unknown;
    };
    match compare_commits(server, &running, candidate).await {
        Ok(ancestry) => map_ancestry(ancestry),
        Err(error) => {
            crate::diagnostics::record("local build", &format!("Ancestry check failed: {error}"));
            CommitRelation::Unknown
        }
    }
}

async fn live_relation(
    application: &AppHandle,
    local: &ManagedLocalBuilds,
    candidate: &str,
) -> CommitRelation {
    let running = state(local).running_commit().map(str::to_owned);
    let Some(running) = running else {
        return CommitRelation::Unknown;
    };
    if candidate.eq_ignore_ascii_case(&running) || candidate.starts_with(&running) {
        return CommitRelation::Same;
    }
    if crate::rosen::paused(application) {
        return CommitRelation::Unknown;
    }
    let Some(project) = velocity_project(application) else {
        return CommitRelation::Unknown;
    };
    let Ok(server) = crate::rosen::observation_server(application, project.project.id) else {
        return CommitRelation::Unknown;
    };
    match compare_commits(&server, &running, candidate).await {
        Ok(ancestry) => map_ancestry(ancestry),
        Err(error) => {
            crate::diagnostics::record("local build", &format!("Ancestry check failed: {error}"));
            CommitRelation::Unknown
        }
    }
}

fn map_ancestry(ancestry: CommitAncestry) -> CommitRelation {
    match ancestry {
        CommitAncestry::Descendant => CommitRelation::Descendant,
        CommitAncestry::Same => CommitRelation::Same,
        CommitAncestry::Ancestor => CommitRelation::Ancestor,
        CommitAncestry::Diverged => CommitRelation::Diverged,
        CommitAncestry::Unknown => CommitRelation::Unknown,
    }
}

/// The Rosen project whose repository is Velocity's own, if this machine has one.
fn velocity_project(application: &AppHandle) -> Option<LandingProject> {
    let store = application.try_state::<ManagedStateStore>()?;
    let store = store.lock().ok()?;
    let project = store.state().projects.iter().find(|project| {
        project.rosen_in_use()
            && (project
                .collapse_repository
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(VELOCITY_REPOSITORY))
                || project
                    .github_remote
                    .as_deref()
                    .is_some_and(|remote| remote.eq_ignore_ascii_case(VELOCITY_REPOSITORY)))
    })?;
    Some(LandingProject {
        project: project.clone(),
        worktrees: store.worktree_preferences(project),
    })
}

async fn build(
    application: &AppHandle,
    local: &ManagedLocalBuilds,
    project: &LandingProject,
    server: &ProjectServer,
    commit: &str,
) {
    let version = state(local).version_for(commit);
    change(application, |state| {
        state.phase = LocalBuildPhase::Building(commit.to_owned());
    });
    let built = match local.builds.prepare(commit) {
        Ok(out_dir) => {
            let release = LocalRelease {
                commit: commit.to_owned(),
                version: version.clone(),
                out_dir,
            };
            local_release::build(&local.app_data, project, &release).await
        }
        Err(error) => Err(error.to_string()),
    };
    let finished = built.and_then(|()| {
        local
            .builds
            .finish(commit, &version)
            .map_err(|error| error.to_string())
    });
    match finished {
        Ok(_) => {
            let builds = local.builds.list();
            let relations = compute_relations(local, server, &builds).await;
            auto_tracker(local).failed_auto = None;
            change(application, |state| state.built(builds, relations));
            crate::project_health::sync_local_build_restart_health(application);
            check_auto_restart(application, AutoRestartScope::All).await;
        }
        Err(reason) => {
            let failure = FailedBuild {
                commit: commit.to_owned(),
                reason,
            };
            // Unsaved, the failure is only forgotten at the next start, which tries again.
            if let Err(error) = local.builds.record_failure(&failure) {
                crate::diagnostics::record("local build", &error.to_string());
            }
            crate::diagnostics::record("local build", &failure.reason);
            change(application, |state| state.build_failed(failure));
        }
    }
}

async fn refresh_existing_relations(application: &AppHandle) {
    if crate::rosen::paused(application) {
        return;
    }
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let Some(project) = velocity_project(application) else {
        return;
    };
    let Ok(server) = crate::rosen::observation_server(application, project.project.id) else {
        return;
    };
    refresh_relations(application, &local, &server).await;
}

/// Restarts into the newest local build by itself when auto-restart is enabled and either
/// it contains every tooling land a waiting issue needs, or Velocity is idle with no turn,
/// land, deploy or local build in progress (#601, #609). Uses the existing Restart path,
/// which saves state, stops Rosen servers, swaps the executable, relaunches and resumes.
async fn check_auto_restart(application: &AppHandle, scope: AutoRestartScope) {
    if cfg!(debug_assertions) || !cfg!(windows) || crate::rosen::paused(application) {
        return;
    }
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let (candidate, enabled, busy) = {
        let current = state(&local);
        (
            current.newest_descendant_commit().map(str::to_owned),
            current.auto_restart(),
            current.busy(),
        )
    };
    let Some(candidate) = candidate.filter(|_| enabled && !busy) else {
        return;
    };
    let (waiting, failed_auto) = {
        let tracker = auto_tracker(&local);
        (tracker.waiting.clone(), tracker.failed_auto.clone())
    };
    let waiting_need_satisfied = waiting_needs_satisfied(application, &candidate, &waiting).await;
    let idle_ready = scope == AutoRestartScope::All && idle_blockers(application).is_idle();
    let now = Instant::now();
    let target = state(&local).should_auto_restart(
        waiting_need_satisfied,
        idle_ready,
        failed_auto.as_ref(),
        now,
    );
    let Some(commit) = target else {
        return;
    };
    let reason = auto_restart_log_line(
        &commit,
        waiting_need_satisfied,
        waiting.len(),
        failed_auto.as_ref(),
    );
    crate::diagnostics::record("local build", &reason);
    if let Err(failure) = perform_restart(application, &commit, false, false).await {
        auto_tracker(&local).failed_auto =
            Some(FailedAutoRestart::new(commit, failure, Instant::now()));
        crate::project_health::sync_local_build_restart_health(application);
    }
}

fn auto_restart_log_line(
    commit: &str,
    waiting_need_satisfied: bool,
    waiting_count: usize,
    failed_auto: Option<&FailedAutoRestart>,
) -> String {
    let short = short_commit(commit);
    if !waiting_need_satisfied {
        return format!("Auto-restarting into {short} (idle)");
    }
    let retrying = failed_auto.is_some_and(|failed| failed.commit.eq_ignore_ascii_case(commit));
    let verb = if retrying {
        "Retrying auto-restart into"
    } else {
        "Auto-restarting into"
    };
    format!("{verb} {short} for {waiting_count} waiting issue(s)")
}

fn idle_blockers(application: &AppHandle) -> IdleBlockers {
    let turn_in_progress = crate::updates::working_agents(application) > 0
        || has_live_turn_in_progress(application)
        || has_startup_recovery_pending(application);
    IdleBlockers {
        turn_in_progress,
        land_in_progress: crate::rosen_landing::is_busy(application),
        deploy_in_progress: crate::rosen_deploy::is_busy(application),
    }
}

fn has_live_turn_in_progress(application: &AppHandle) -> bool {
    application
        .try_state::<crate::threads::ManagedThreads>()
        .and_then(|threads| {
            threads
                .lock()
                .ok()
                .map(|guard| guard.has_any_turn_in_progress())
        })
        .unwrap_or(false)
}

fn has_startup_recovery_pending(application: &AppHandle) -> bool {
    application
        .try_state::<ManagedStateStore>()
        .and_then(|store| {
            store.lock().ok().map(|guard| {
                guard
                    .state()
                    .projects
                    .iter()
                    .flat_map(|project| project.threads.iter())
                    .any(|thread| thread.startup_recovery_pending)
            })
        })
        .unwrap_or(false)
}

async fn waiting_needs_satisfied(
    application: &AppHandle,
    candidate: &str,
    waiting: &[WaitingIssueNeed],
) -> bool {
    if waiting.is_empty() {
        return false;
    }
    let Some(project) = velocity_project(application) else {
        return false;
    };
    let Ok(server) = crate::rosen::observation_server(application, project.project.id) else {
        return false;
    };
    for need in waiting {
        if need.required_commits.is_empty() {
            continue;
        }
        let mut all_contained = true;
        for required in &need.required_commits {
            if !matches!(
                commit_contains(&server, candidate, required).await,
                Ok(true)
            ) {
                all_contained = false;
                break;
            }
        }
        if all_contained {
            return true;
        }
    }
    false
}

fn target_build(local: &ManagedLocalBuilds, commit: &str) -> Option<LocalBuild> {
    let state = state(local);
    if state.busy() {
        return None;
    }
    state.kept_build(commit).cloned()
}

/// Applies the install-time ancestry guard and claims the restart atomically.
fn begin_restart(
    application: &AppHandle,
    build: &LocalBuild,
    relation: CommitRelation,
    confirmed: bool,
    rollback: bool,
) -> BeginRestart {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return BeginRestart::Busy;
    };
    let outcome = {
        let mut state = state(&local);
        if state.busy() {
            return BeginRestart::Busy;
        }
        match state.install_decision(&build.commit, relation, confirmed, rollback) {
            InstallDecision::Allow => {
                state.phase = LocalBuildPhase::Restarting(build.commit.clone());
                BeginRestart::Proceed
            }
            InstallDecision::Confirm(prompt) => {
                crate::diagnostics::record(
                    "local build",
                    &format!(
                        "ConfirmRestart for {}: {}",
                        short_commit(&build.commit),
                        prompt.message
                    ),
                );
                let message = prompt.message.clone();
                state.phase = LocalBuildPhase::ConfirmRestart(prompt);
                BeginRestart::Confirm(message)
            }
            InstallDecision::Refuse(reason) => {
                crate::diagnostics::record(
                    "local build",
                    &format!("Refuse for {}: {reason}", short_commit(&build.commit)),
                );
                state.phase = LocalBuildPhase::RestartFailed(reason.clone());
                BeginRestart::Refuse(reason)
            }
        }
    };
    publish(application);
    outcome
}

async fn restart(
    application: &AppHandle,
    build: &LocalBuild,
    rollback: bool,
) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err(
            "Development builds don't restart into local builds. Use a release build.".into(),
        );
    }
    let local = application
        .try_state::<ManagedLocalBuilds>()
        .ok_or("Local builds haven't started yet")?;
    let previous_commit = state(&local).running_commit().map(str::to_owned);
    let current_exe = std::env::current_exe().ok();
    let (builds, read) = (local.builds.clone(), build.clone());
    let (bytes, guard_error) = tauri::async_runtime::spawn_blocking(move || {
        let bytes = builds
            .read_executable(&read)
            .map_err(|error| error.to_string())?;
        let guard_error = if rollback {
            builds.clear_pending_restart();
            None
        } else {
            builds
                .arm_pending_restart(
                    &read.commit,
                    previous_commit.as_deref(),
                    current_exe.as_deref(),
                )
                .err()
                .map(|error| error.to_string())
        };
        Ok::<_, String>((bytes, guard_error))
    })
    .await
    .map_err(|error| error.to_string())??;
    if let Some(error) = guard_error {
        crate::diagnostics::record(
            "local build",
            &format!("Could not arm startup rollback guard: {error}"),
        );
    }
    let folder = local.builds.folder_of(&build.commit);
    let result = crate::updates::hand_over(application, "Velocity was not restarted", || {
        // A copy installed where Velocity may not write can't be swapped in place.
        crate::updates::restart_portable(application, &bytes).map_err(|error| {
            format!(
                "{error}. The setup for this build is in {}.",
                folder.display()
            )
        })
    })
    .await;
    if result.is_err() && !rollback {
        local.builds.clear_pending_restart();
    }
    result
}

/// Verifies a freshly restarted local build's startup and renderer heartbeats for
/// [`POST_RESTART_HEALTH_WINDOW`]. Rolls back to the previous build and marks the new
/// commit as failed if startup or heartbeats fail (#609).
async fn monitor_post_restart_health(application: AppHandle, pending: PendingRestart) {
    let short = short_commit(&pending.commit).to_owned();
    crate::diagnostics::record(
        "local build",
        &format!("Verifying restarted build {short} startup and heartbeat"),
    );
    let started = Instant::now();
    while started.elapsed() < POST_RESTART_HEALTH_WINDOW {
        tokio::time::sleep(POST_RESTART_HEALTH_POLL).await;
        if let Some(detail) = post_restart_health_failure(&application, started.elapsed()) {
            rollback_failed_restart(&application, &pending, detail).await;
            return;
        }
    }
    if let Some(local) = application.try_state::<ManagedLocalBuilds>() {
        local.builds.clear_pending_restart();
    }
    crate::diagnostics::record(
        "local build",
        &format!("Restarted build {short} passed startup and heartbeat verification"),
    );
}

fn post_restart_health_failure(application: &AppHandle, elapsed: Duration) -> Option<&'static str> {
    let Some(window) = application.get_webview_window(crate::window::MAIN_WINDOW) else {
        return (elapsed >= STARTUP_WINDOW_GRACE)
            .then_some("main window did not open after restart");
    };
    let visible = window.is_visible().unwrap_or(true) && !window.is_minimized().unwrap_or(false);
    application
        .state::<crate::window::WindowHeartbeat>()
        .post_restart_failure(elapsed, visible)
}

async fn rollback_failed_restart(application: &AppHandle, pending: &PendingRestart, detail: &str) {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let target_short = short_commit(&pending.commit).to_owned();
    let prev_label = pending
        .previous_commit
        .as_deref()
        .map(short_commit)
        .unwrap_or("previous")
        .to_owned();
    let reason = format!("Restarted build {target_short} failed health check: {detail}");
    crate::diagnostics::record(
        "local build",
        &format!("Rolling back {target_short} to {prev_label}: {detail}"),
    );
    change(application, |state| {
        state.phase = LocalBuildPhase::Restarting(prev_label.clone());
    });
    let failure = FailedBuild {
        commit: pending.commit.clone(),
        reason,
    };
    let (builds, pending_copy, failure_copy, short_for_task) = (
        local.builds.clone(),
        pending.clone(),
        failure.clone(),
        target_short.clone(),
    );
    let prepared = tauri::async_runtime::spawn_blocking(move || {
        let bytes = builds
            .read_rollback_executable(&pending_copy)
            .map_err(|error| error.to_string());
        if let Err(error) = builds.mark_build_failed(&failure_copy) {
            crate::diagnostics::record(
                "local build",
                &format!("Could not mark {short_for_task} failed: {error}"),
            );
        }
        builds.clear_pending_restart();
        bytes
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    let rollback_result = match prepared {
        Ok(bytes) => {
            crate::updates::hand_over(application, "Velocity could not roll back", || {
                crate::updates::restart_portable(application, &bytes)
            })
            .await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = rollback_result {
        crate::diagnostics::record(
            "local build",
            &format!("Rollback from {target_short} to {prev_label} failed: {error}"),
        );
        change(application, |state| state.build_failed(failure));
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Applies `update` and tells the window when what the title bar shows changed.
fn change(application: &AppHandle, update: impl FnOnce(&mut LocalBuildState)) {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let changed = {
        let mut state = state(&local);
        let before = state.view();
        update(&mut state);
        before != state.view()
    };
    if changed {
        publish(application);
    }
}

fn view(application: &AppHandle) -> LocalBuildView {
    application
        .try_state::<ManagedLocalBuilds>()
        .map(|local| state(&local).view())
        .unwrap_or_default()
}

fn publish(application: &AppHandle) {
    let view = view(application);
    if let Err(error) = (LocalBuildStatusChanged { view }).emit(application) {
        eprintln!("Could not publish the local build status: {error}");
    }
}
