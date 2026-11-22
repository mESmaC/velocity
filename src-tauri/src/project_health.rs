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
    BaseCiState, CommitOrder, OpenRepair, RepairThreadState, format_ci_repair_action,
};
use velocity_core::collapse::{
    COLLAPSE_FORWARDING_SIGNAL, ForwarderState, collapse_forwarding_needs_attention,
};
use velocity_core::project_health::{
    AIRC_ISSUE_FAILURE_TTL_SECONDS, DISPATCH_IDLE_LIMIT_SECONDS, HealthSeverity, HealthSignalKind,
    INTEGRATION_UNCOVERED_LANDS, INTEGRATION_UNCOVERED_SECONDS, MIRROR_BEHIND_LIMIT_SECONDS,
    POST_LAND_STALL_LIMIT_SECONDS, ProjectHealthItem, ProjectHealthView,
    REVIEW_STUCK_LIMIT_SECONDS, airc_issue_tool_failure_item, format_duration,
    format_stopped_check_detail, short_commit,
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
    issue_number_from_piece, stale_claim_reason, summarize_skipped_issues,
};
use velocity_threads::landing_runner::refusal_instructions;

use crate::continuous_dispatch::DispatchHealthSnapshot;
use crate::rosen_mirror::MirrorRetryOutcome;

const SOURCE: &str = "project health";
const COLLAPSE_FORWARDING_KEY: &str = "collapse-forwarding";
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(60);
const MIRROR_RETRY_COOLDOWN_SECS: u64 = 10 * 60;
const REVIEW_CHECK_COOLDOWN_SECS: u64 = 10 * 60;
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
    free_slot_since: Option<u64>,
    last_seen_dispatch: Option<u64>,
    last_logged_skip: Option<String>,
    parked: BTreeMap<u32, ParkedRecord>,
    stopped_check: Option<StoppedCheckRecord>,
    acted_times: HashMap<String, (String, String, u64)>,
    release_health: Option<ReleaseHealthCache>,
    pending_release: Option<PendingRelease>,
    retried_release_runs: HashSet<u64>,
    collapse_forwarding: Option<CollapseForwardingRecord>,
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
    let items = application
        .try_state::<ManagedProjectHealth>()
        .map(|managed| {
            registry(&managed)
                .views
                .get(&project_identifier)
                .cloned()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    ProjectHealthView {
        project_identifier,
        items,
    }
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
/// the "GitHub is not receiving lands: finish Collapse forwarding" project health row (#579).
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
/// state or the project's GitHub repository changes, clearing it once in sync (#579).
pub(crate) fn sync_collapse_forwarding(
    application: &AppHandle,
    project: u32,
    collapse_repository: Option<&str>,
    github_remote: Option<&str>,
    forwarder: Option<ForwarderState>,
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
    items.extend(check_deploy_health(project, &server, now_ms).await);
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

/// Records that a `[deploy.<step>]` command passed on `commit` (#594).
pub(crate) fn record_deploy_passed(
    application: &AppHandle,
    project: u32,
    step: &str,
    commit: &str,
) {
    let now_ms = unix_millis();
    let now = now_ms / 1000;
    let item = ProjectHealthItem::new(
        deploy_health_key(step),
        HealthSignalKind::Deploy,
        HealthSeverity::Info,
        format!("Deployed {step} at {}", short_commit(commit)),
        "Deploy step passed",
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
        .filter_map(|run| deploy_run_health_item(project, &run, now_ms))
        .collect()
}

fn deploy_run_health_item(
    project: &ProjectItem,
    run: &velocity_rosen::deploy::DeployRunRecord,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let acted_at = run
        .submitted_at
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(now_ms);
    match run.state {
        velocity_core::deploy::DeployRunState::Running => Some(ProjectHealthItem::new(
            deploy_health_key(&run.step),
            HealthSignalKind::Deploy,
            HealthSeverity::Info,
            format!("Deploying {} at {}", run.step, short_commit(&run.commit)),
            "Running [deploy] step in base checkout",
            acted_at,
        )),
        velocity_core::deploy::DeployRunState::Failed => {
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
            Some(
                ProjectHealthItem::new(
                    deploy_health_key(&run.step),
                    HealthSignalKind::Deploy,
                    HealthSeverity::Error,
                    format!(
                        "Deploy `{}` failed at {}",
                        run.step,
                        short_commit(&run.commit)
                    ),
                    action,
                    acted_at,
                )
                .with_detail(velocity_core::deploy::summarize_output(&run.output, 180))
                .with_thread(thread),
            )
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

/// Signal: GitHub is not receiving lands until Collapse downstream forwarding finishes (#579).
fn check_collapse_forwarding(
    application: &AppHandle,
    project: &ProjectItem,
    now_ms: u64,
) -> Option<ProjectHealthItem> {
    let forwarder = project
        .collapse_mirror_health
        .as_ref()
        .map(|health| health.forwarder);
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
        record.as_ref(),
        now_ms,
    ))
}

fn collapse_forwarding_item(
    collapse_repository: Option<&str>,
    github_repository: Option<&str>,
    record: Option<&CollapseForwardingRecord>,
    now_ms: u64,
) -> ProjectHealthItem {
    let (action, detail, acted_at_ms) = match record {
        Some(entry) => {
            let trimmed = entry.reason.trim();
            let action = if entry.opened_setup_page {
                if trimmed.is_empty() {
                    "Opened the Collapse GitHub App setup page; grant Contents and Workflows write and finish forwarding on Collapse".to_owned()
                } else {
                    format!("Opened setup page · {trimmed}")
                }
            } else if trimmed.is_empty() {
                "Install the Collapse GitHub App with Contents and Workflows write and finish forwarding on Collapse".to_owned()
            } else {
                trimmed.to_owned()
            };
            let detail = Some(format!(
                "{} → {}",
                entry.collapse_repository, entry.github_repository
            ));
            (action, detail, entry.acted_at_ms)
        }
        None => {
            let detail = match (collapse_repository, github_repository) {
                (Some(collapse), Some(github)) => Some(format!("{collapse} → {github}")),
                (_, Some(github)) => Some(github.to_owned()),
                (Some(collapse), None) => Some(collapse.to_owned()),
                (None, None) => None,
            };
            (
                "Collapse has no downstream remote configured for this GitHub repository"
                    .to_owned(),
                detail,
                now_ms,
            )
        }
    };
    ProjectHealthItem::new(
        COLLAPSE_FORWARDING_KEY,
        HealthSignalKind::CollapseForwarding,
        HealthSeverity::Warning,
        COLLAPSE_FORWARDING_SIGNAL,
        action,
        acted_at_ms,
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
        let action =
            format_ci_repair_action(open.as_ref(), ci.commit.as_deref(), project.spec_locked);
        let commit = ci
            .commit
            .as_deref()
            .map(|id| format!(" at {}", short_commit(id)))
            .unwrap_or_default();
        let detail = (!ci.failing.is_empty()).then(|| ci.failing.join(" · "));
        items.push(
            ProjectHealthItem::new(
                "base-red:ci",
                HealthSignalKind::BaseRed,
                HealthSeverity::Error,
                format!("GitHub CI red on base{commit}"),
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
            let _ = crate::rosen_events::record_landed(
                application,
                project,
                change,
                thread.rosen_progress_generation,
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

/// Signal 7: Slots free and ready/open issues, but nothing dispatched for 10 min (#509).
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
    let has_candidates = dispatch.ready_count > 0
        || !dispatch.skipped.is_empty()
        || dispatch.backlog_error.is_some();
    if !has_free_slot || !has_candidates {
        with_tracker(application, project.id, |tracker| {
            tracker.free_slot_since = None;
            tracker.last_logged_skip = None;
        });
        return None;
    }
    let since = with_tracker(application, project.id, |tracker| {
        if dispatch.last_dispatched_secs != tracker.last_seen_dispatch {
            tracker.last_seen_dispatch = dispatch.last_dispatched_secs;
            tracker.free_slot_since = Some(now);
            tracker.last_logged_skip = None;
        }
        *tracker.free_slot_since.get_or_insert(now)
    });
    let waited = now.saturating_sub(since);
    if waited < DISPATCH_IDLE_LIMIT_SECONDS {
        return None;
    }
    let detail = dispatch_idle_detail(project, dispatch);
    let should_log = with_tracker(application, project.id, |tracker| {
        if tracker.last_logged_skip.as_deref() == Some(detail.as_str()) {
            false
        } else {
            tracker.last_logged_skip = Some(detail.clone());
            true
        }
    });
    if should_log {
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "{}: slots free ({busy}/{slots}) for {} with nothing dispatched · {detail}",
                project.name,
                format_duration(waited)
            ),
        );
    }
    let action = if !dispatch.skipped.is_empty() {
        format!(
            "Logged why {} open issue(s) were skipped",
            dispatch.skipped.len()
        )
    } else {
        "Logged why continuous dispatch is idle".into()
    };
    Some(
        ProjectHealthItem::new(
            "dispatch-idle",
            HealthSignalKind::DispatchIdle,
            HealthSeverity::Warning,
            format!(
                "Slots free ({busy}/{slots}) for {} with nothing dispatched",
                format_duration(waited)
            ),
            action,
            now_ms,
        )
        .with_detail(Some(detail)),
    )
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
    let green = if project.collapse_repository.is_some() {
        velocity_rosen::release::release_commit(server, Some(&pending.commit))
            .await
            .ok()
            .map(|release| release.green)
    } else if let Some(repository) = repository {
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
    let view = ProjectHealthView {
        project_identifier: project,
        items,
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
