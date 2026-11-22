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
    CommitRelation, FailedBuild, InstallDecision, LocalBuild, LocalBuildPhase, LocalBuildSettings,
    LocalBuildState, LocalBuildView, LocalBuilds, short_commit,
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
/// How often idle duration and waiting-issue auto-restart readiness are sampled (#601).
const AUTO_RESTART_POLL: Duration = Duration::from_secs(10);
/// Auto-restart into the newest local build after 2 minutes with no running agent turn (#601).
const IDLE_AUTO_RESTART: Duration = Duration::from_secs(2 * 60);
/// What a build needs free before it starts: two compiler jobs at about 1.5 GB each
/// (`scripts/release-local.ps1`) and the link.
const BUILD_MEMORY: u64 = 4 * 1024 * 1024 * 1024;

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
    idle_since: Option<Instant>,
    waiting: Vec<WaitingIssueNeed>,
    failed_commit: Option<String>,
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
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(INITIAL_RELATION_REFRESH).await;
        refresh_existing_relations(&handle).await;
        check_auto_restart(&handle).await;
        let mut next_look = Instant::now() + FIRST_LOOK.saturating_sub(INITIAL_RELATION_REFRESH);
        loop {
            tokio::time::sleep(AUTO_RESTART_POLL).await;
            if Instant::now() >= next_look {
                look(&handle).await;
                next_look = Instant::now() + LOOK_INTERVAL;
            }
            check_auto_restart(&handle).await;
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
    if changed && !waiting.is_empty() && !cfg!(debug_assertions) && cfg!(windows) {
        let handle = application.clone();
        tauri::async_runtime::spawn(async move {
            check_auto_restart(&handle).await;
        });
    }
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
        auto_tracker(&local).failed_commit = None;
        if !cfg!(debug_assertions) && cfg!(windows) {
            let handle = application.clone();
            tauri::async_runtime::spawn(async move {
                check_auto_restart(&handle).await;
            });
        }
    }
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

async fn perform_restart(
    application: &AppHandle,
    commit: &str,
    confirmed: bool,
    rollback: bool,
) -> Result<(), String> {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return Err("Local builds haven't started yet".into());
    };
    let Some(build) = target_build(&local, commit) else {
        return Err(format!(
            "Local build {} is no longer available",
            short_commit(commit)
        ));
    };
    let relation = live_relation(application, &local, &build.commit).await;
    if begin_restart(application, &build, relation, confirmed, rollback)
        && let Err(message) = restart(application, &build).await
    {
        change(application, |state| {
            state.phase = LocalBuildPhase::RestartFailed(message.clone());
        });
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
            && project
                .github_remote
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(VELOCITY_REPOSITORY))
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
            auto_tracker(local).failed_commit = None;
            change(application, |state| state.built(builds, relations));
            check_auto_restart(application).await;
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
/// it contains every tooling land a waiting issue needs, or no agent turn has been running
/// for 2 minutes (#601). Uses the existing Restart path, which saves state and resumes turns.
async fn check_auto_restart(application: &AppHandle) {
    if cfg!(debug_assertions) || !cfg!(windows) || crate::rosen::paused(application) {
        return;
    }
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let idle_ready = update_idle_ready(application, &local);
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
        (tracker.waiting.clone(), tracker.failed_commit.clone())
    };
    let waiting_need_satisfied = waiting_needs_satisfied(application, &candidate, &waiting).await;
    let target = state(&local).should_auto_restart(
        waiting_need_satisfied,
        idle_ready,
        failed_auto.as_deref(),
    );
    let Some(commit) = target else {
        return;
    };
    let reason = if waiting_need_satisfied {
        format!(
            "Auto-restarting into {} for {} waiting issue(s)",
            short_commit(&commit),
            waiting.len()
        )
    } else {
        format!(
            "Auto-restarting into {} after 2 minutes with no running agent turn",
            short_commit(&commit)
        )
    };
    crate::diagnostics::record("local build", &reason);
    if perform_restart(application, &commit, false, false)
        .await
        .is_err()
    {
        auto_tracker(&local).failed_commit = Some(commit);
    }
}

fn update_idle_ready(application: &AppHandle, local: &ManagedLocalBuilds) -> bool {
    let mut tracker = auto_tracker(local);
    if crate::updates::working_agents(application) > 0 {
        tracker.idle_since = None;
        return false;
    }
    let since = *tracker.idle_since.get_or_insert_with(Instant::now);
    since.elapsed() >= IDLE_AUTO_RESTART
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
) -> bool {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return false;
    };
    let proceed = {
        let mut state = state(&local);
        if state.busy() {
            return false;
        }
        match state.install_decision(&build.commit, relation, confirmed, rollback) {
            InstallDecision::Allow => {
                state.phase = LocalBuildPhase::Restarting(build.commit.clone());
                true
            }
            InstallDecision::Confirm(prompt) => {
                state.phase = LocalBuildPhase::ConfirmRestart(prompt);
                false
            }
            InstallDecision::Refuse(reason) => {
                crate::diagnostics::record("local build", &reason);
                state.phase = LocalBuildPhase::RestartFailed(reason);
                false
            }
        }
    };
    publish(application);
    proceed
}

async fn restart(application: &AppHandle, build: &LocalBuild) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err(
            "Development builds don't restart into local builds. Use a release build.".into(),
        );
    }
    let local = application
        .try_state::<ManagedLocalBuilds>()
        .ok_or("Local builds haven't started yet")?;
    let (builds, read) = (local.builds.clone(), build.clone());
    let bytes = tauri::async_runtime::spawn_blocking(move || builds.read_executable(&read))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let folder = local.builds.folder_of(&build.commit);
    crate::updates::hand_over(application, "Velocity was not restarted", || {
        // A copy installed where Velocity may not write can't be swapped in place.
        crate::updates::restart_portable(application, &bytes).map_err(|error| {
            format!(
                "{error}. The setup for this build is in {}.",
                folder.display()
            )
        })
    })
    .await
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
