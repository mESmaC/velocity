//! Native background adapter for F8 continuous dispatch. It never uses the selected
//! project's issue rail, so changing projects cannot redirect a background launch.
mod holds;
pub(crate) mod overlap;
mod parked;
pub(crate) mod reopened_land;
pub(crate) mod restart_wait;
mod text_blockers;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use velocity_core::project_health::BlockedDispatchIssue;
use velocity_github::{
    DependencyGraph, ExternalIssueState, GitHubIssue, IssueClient, IssueHost, IssueState,
    REMOTE_PROGRESS_WINDOW_SECONDS,
};
use velocity_store::continuous_dispatch::has_capacity as project_has_capacity;
use velocity_store::continuous_dispatch::{
    RemoteWorkHold, busy_count, can_finish_empty_dispatch, idle_nudge_exhausted,
};
use velocity_store::issues::ThreadIssue;
use velocity_store::{ProjectItem, TaskAttribution, VersionControl};
use velocity_threads::continuous_dispatch::{
    AttemptClearReason, DispatchView, DispatchWork, SkippedIssue, TextBlockerContext,
    attempts_to_clear_with_context, blocked_dispatch_issue, dispatchable_issue_with_blockers,
    dispatchable_task, has_unsatisfied_blockers_with_context, integration_notice,
    issue_number_from_piece, issue_skip_reason_with_blockers,
    open_referenced_blockers_for_issue_with_context, ordered_work_with_blockers, plan_task,
    stale_claim_reason, stalled_landed_issues, uncovered_base_notice,
};

#[derive(Clone, Default)]
struct Backlog {
    repository: String,
    issues: Vec<GitHubIssue>,
    comments: BTreeMap<u32, Vec<String>>,
    error: Option<String>,
    refreshed_at: Option<Instant>,
}

type Backlogs = Mutex<BTreeMap<u32, Backlog>>;

/// Which open issues wait on which across every project's repository (#510).
type Dependencies = Mutex<DependencyGraph>;

/// What a project's integration record said when it was last read.
#[derive(Clone, Default)]
struct IntegrationRecord {
    uncovered: velocity_rosen::integration::Uncovered,
    last_build: Option<String>,
}

type IntegrationRecords = Mutex<BTreeMap<u32, (Instant, IntegrationRecord)>>;

/// How long a read of the integration record serves the Duty view, which asks every two
/// seconds. A build that passes clears the notice within this.
const INTEGRATION_RECORD_REFRESH: Duration = Duration::from_secs(60);

/// Issues in a project dispatch does not read can still wait on a ready issue. Their
/// dependencies change rarely, so these projects are read far less often than a backlog.
const DEPENDENCY_REFRESH: Duration = Duration::from_secs(10 * 60);

/// How many retired dependency holds one issue read converts (#511). Each takes several
/// GitHub requests, and the dispatch pass waits for them.
const HOLDS_PER_READ: usize = 5;
/// How long a hold that could not be converted waits before the next attempt.
const HOLD_RETRY: Duration = Duration::from_secs(60 * 60);
/// Grace period for a freshly posted land comment whose mirror push is still in flight (#519).
const MIRROR_LAG_HOLD_SECONDS: u64 = 10 * 60;

/// How many open issues each of `repository`'s issues unblocks, for the dispatch order and
/// the Work pool card. Empty until the first issue read.
pub(crate) fn unblock_counts(
    application: &AppHandle,
    repository: &str,
) -> Result<BTreeMap<u32, u32>, String> {
    let Some(dependencies) = application.try_state::<Dependencies>() else {
        return Ok(BTreeMap::new());
    };
    Ok(lock(dependencies.inner(), "issue dependencies")?.unblock_counts(repository))
}

#[tauri::command]
#[specta::specta]
pub async fn get_continuous_dispatch(
    application: AppHandle,
    project_identifier: u32,
) -> Result<DispatchView, String> {
    crate::layout::run_concurrently("get_continuous_dispatch", move || {
        view(&application, project_identifier)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_continuous_dispatch(
    application: AppHandle,
    project_identifier: u32,
    slots: u32,
    paused: bool,
) -> Result<DispatchView, String> {
    crate::layout::off_ui_thread_named("set_continuous_dispatch", move || {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .set_continuous_dispatch(project_identifier, slots, paused)
        .map_err(|error| error.to_string())?;
        view(&application, project_identifier)
    })
    .await
}

fn view(application: &AppHandle, identifier: u32) -> Result<DispatchView, String> {
    let backlog = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
        .get(&identifier)
        .cloned();
    let issues_known = backlog
        .as_ref()
        .is_some_and(|backlog| backlog.error.is_none());
    let backlog = backlog.unwrap_or_default();
    let blocker_context = text_blockers::context_for_project(application, identifier, None);
    let mut claimed = claimed_pieces(application, identifier)?;
    let unblocks = unblock_counts(application, &backlog.repository)?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store.project(identifier).ok_or("The project was removed")?;
    include_overlap_waits(project, &backlog.repository, &mut claimed);
    include_remote_work_holds(project, &backlog.repository, &backlog.issues, &mut claimed);
    restart_wait::include_restart_waits(application, identifier, &mut claimed);
    let work = ordered_work_with_blockers(
        &store.state().projects,
        project,
        &backlog.repository,
        &backlog.issues,
        &claimed,
        issues_known,
        &unblocks,
        &blocker_context,
    );
    let mut view = DispatchView::new(project, &work, backlog.error);
    let stalled = stalled_landed_issues(
        project,
        &backlog.repository,
        &backlog.issues,
        unix_seconds(),
    );
    // Only a project Rosen serves has an integration record to read.
    let rosen = project.rosen_in_use();
    drop(store);
    let record = if rosen {
        integration_record(application, identifier)
    } else {
        IntegrationRecord::default()
    };
    // An integration build that stops covering the base is said here, not left silent (#503).
    let uncovered = uncovered_base_notice(
        record.uncovered.lands,
        record.uncovered.since,
        unix_seconds(),
    );
    view.integration_notice =
        integration_notice(uncovered.as_deref(), &stalled, record.last_build.as_deref());
    Ok(view)
}

/// The project's integration record, read again once the last read is a minute old.
fn integration_record(application: &AppHandle, identifier: u32) -> IntegrationRecord {
    let records = application.state::<IntegrationRecords>();
    if let Ok(known) = lock(records.inner(), "integration records")
        && let Some((read, record)) = known.get(&identifier)
        && read.elapsed() < INTEGRATION_RECORD_REFRESH
    {
        return record.clone();
    }
    let record = read_integration_record(application, identifier);
    if let Ok(mut known) = lock(records.inner(), "integration records") {
        known.insert(identifier, (Instant::now(), record.clone()));
    }
    record
}

/// A missing or unreadable integration record still shows a wait, without a reason, and
/// reports no uncovered lands.
fn read_integration_record(application: &AppHandle, identifier: u32) -> IntegrationRecord {
    let Ok(data) = application.path().app_data_dir() else {
        return IntegrationRecord::default();
    };
    let server = velocity_rosen::ProjectServer::new(&data, identifier);
    IntegrationRecord {
        uncovered: velocity_rosen::integration::uncovered(&server).unwrap_or_default(),
        last_build: velocity_rosen::integration::last_outcome(&server)
            .ok()
            .flatten(),
    }
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn integration_idle(application: &AppHandle, identifier: u32) -> Result<bool, String> {
    let known = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
        .get(&identifier)
        .filter(|backlog| backlog.error.is_none())
        .cloned();
    let Some(backlog) = known else {
        return Ok(false);
    };
    let blocker_context = text_blockers::context_for_project(application, identifier, None);
    let mut claimed = claimed_pieces(application, identifier)?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(project) = store.project(identifier) else {
        return Ok(false);
    };
    include_remote_work_holds(project, &backlog.repository, &backlog.issues, &mut claimed);
    restart_wait::include_restart_waits(application, identifier, &mut claimed);
    Ok(busy_count(project) == 0
        && ordered_work_with_blockers(
            &store.state().projects,
            project,
            &backlog.repository,
            &backlog.issues,
            &claimed,
            true,
            // Only emptiness matters here, not the order.
            &BTreeMap::new(),
            &blocker_context,
        )
        .is_empty())
}

/// When continuous dispatch last started a task or issue for each project, in Unix seconds (#509).
type LastDispatched = Mutex<BTreeMap<u32, u64>>;

/// Projects whose cached backlog should be refreshed on the next monitor pass (for example
/// right after the owner lifts a hold or relabels an issue from project health, #616).
type ForceBacklogRefresh = Mutex<HashSet<u32>>;

/// Project-level continuous dispatch state for project health checks (#509, #616).
#[derive(Clone, Debug, Default)]
pub(crate) struct DispatchHealthSnapshot {
    pub backlog_error: Option<String>,
    pub open_issues: usize,
    pub ready_count: usize,
    pub skipped: Vec<SkippedIssue>,
    pub blocked_issues: Vec<BlockedDispatchIssue>,
    pub stale_released: Vec<(u32, String)>,
    pub stalled_landed: Vec<u32>,
    pub last_dispatched_secs: Option<u64>,
}

/// Reads the current dispatch backlog, skip reasons, stalled landed issues and last dispatch
/// time for `identifier` without issuing any GitHub requests (#509).
pub(crate) fn health_snapshot(
    application: &AppHandle,
    identifier: u32,
    now: u64,
) -> Option<DispatchHealthSnapshot> {
    let backlog = application
        .try_state::<Backlogs>()
        .and_then(|backlogs| lock(backlogs.inner(), "dispatch backlog").ok())
        .and_then(|backlogs| backlogs.get(&identifier).cloned())
        .unwrap_or_default();
    let last_dispatched_secs = application
        .try_state::<LastDispatched>()
        .and_then(|times| lock(times.inner(), "last dispatched").ok())
        .and_then(|times| times.get(&identifier).copied());
    let (live_claims, reaped) =
        crate::airc_claims::live_dispatch_claims(application, identifier).unwrap_or_default();
    let mut claimed: HashSet<String> = live_claims
        .iter()
        .map(|claim| claim.key().to_owned())
        .collect();
    let stale_released: Vec<(u32, String)> = reaped
        .iter()
        .filter_map(|item| {
            issue_number_from_piece(&item.piece)
                .map(|number| (number, stale_claim_reason(&item.nick)))
        })
        .collect();
    let unblocks = unblock_counts(application, &backlog.repository).unwrap_or_default();
    let blocker_context = text_blockers::context_for_project(application, identifier, None);
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let project = store.project(identifier)?;
    let issues_known = backlog.error.is_none();
    let stalled_landed = stalled_landed_issues(project, &backlog.repository, &backlog.issues, now);
    let restart_waits = restart_wait::waits_for_project(application, identifier);
    let mut skipped = Vec::new();
    let mut blocked_issues = Vec::new();
    if issues_known {
        for issue in &backlog.issues {
            let reason = if let Some(wait) = restart_waits.get(&issue.number) {
                Some(wait.skipped())
            } else {
                issue_skip_reason_with_blockers(
                    &store.state().projects,
                    project,
                    &backlog.repository,
                    issue,
                    &claimed,
                    now,
                    &blocker_context,
                )
            };
            if let Some(skip) = reason {
                if let Some(blocked) =
                    blocked_dispatch_issue(project, &backlog.repository, issue, &skip)
                {
                    blocked_issues.push(blocked);
                }
                skipped.push(skip);
            }
        }
    }
    include_overlap_waits(project, &backlog.repository, &mut claimed);
    include_launch_holds(project, &backlog.repository, &mut claimed);
    include_remote_work_holds(project, &backlog.repository, &backlog.issues, &mut claimed);
    restart_wait::include_restart_waits(application, identifier, &mut claimed);
    let ready_count = ordered_work_with_blockers(
        &store.state().projects,
        project,
        &backlog.repository,
        &backlog.issues,
        &claimed,
        issues_known,
        &unblocks,
        &blocker_context,
    )
    .len();
    Some(DispatchHealthSnapshot {
        backlog_error: backlog.error,
        open_issues: backlog.issues.len(),
        ready_count,
        skipped,
        blocked_issues,
        stale_released,
        stalled_landed,
        last_dispatched_secs,
    })
}

/// Replaces one cached backlog issue in place and schedules an immediate backlog refresh
/// on the next monitor tick so owner unblock actions take effect right away (#616).
pub(crate) fn update_cached_issue_and_refresh(
    application: &AppHandle,
    identifier: u32,
    issue: GitHubIssue,
) {
    if let Some(backlogs) = application.try_state::<Backlogs>()
        && let Ok(mut backlogs) = lock(backlogs.inner(), "dispatch backlog")
        && let Some(backlog) = backlogs.get_mut(&identifier)
        && let Some(slot) = backlog
            .issues
            .iter_mut()
            .find(|existing| existing.number == issue.number)
    {
        *slot = issue;
    }
    if let Some(forced) = application.try_state::<ForceBacklogRefresh>()
        && let Ok(mut forced) = lock(forced.inner(), "backlog refresh")
    {
        forced.insert(identifier);
    }
}

fn record_dispatched(application: &AppHandle, identifier: u32) {
    let now = unix_seconds();
    if let Some(times) = application.try_state::<LastDispatched>()
        && let Ok(mut times) = lock(times.inner(), "last dispatched")
    {
        times.insert(identifier, now);
    }
    crate::project_health::notify_dispatched(application, identifier, now);
}

pub fn initialize(application: &AppHandle) {
    application.manage(Backlogs::default());
    application.manage(Dependencies::default());
    application.manage(IntegrationRecords::default());
    application.manage(LastDispatched::default());
    application.manage(ForceBacklogRefresh::default());
    reopened_land::initialize(application);
    restart_wait::initialize(application);
    // Without its executable or data folder Velocity cannot start a bounded review; a
    // parked issue then returns to dispatch when its report passes the limit (#506).
    if let (Ok(executable), Ok(data)) = (std::env::current_exe(), application.path().app_data_dir())
    {
        application.manage(velocity_threads::parked_review::ParkedReviews::new(
            executable, data,
        ));
    }
    let application = application.clone();
    std::thread::spawn(move || monitor(application));
}

/// How often continuous dispatch polls GitHub's shared backlog cache.
const GITHUB_BACKLOG_REFRESH: Duration = Duration::from_secs(30);
/// How often continuous dispatch polls Collapse's no-wake edge issue cursor (#589).
const COLLAPSE_CURSOR_REFRESH: Duration = Duration::from_secs(5);

struct Reader {
    directory: String,
    host: IssueHost,
    remote: Option<String>,
    client: Option<IssueClient>,
    refreshed: Option<Instant>,
    report_retry: BTreeMap<u32, Instant>,
    /// Issues whose retired dependency hold could not be converted, and when that failed.
    hold_retry: BTreeMap<u32, Instant>,
    /// Cached comment bodies per attempted issue `(updated_at, comments)` for blocker-close retry (#617).
    attempt_comments: BTreeMap<u32, (String, Vec<String>)>,
    backlog: Backlog,
    parked: parked::Parked,
    holds: holds::Holds,
    text_blockers: text_blockers::TextBlockers,
}

fn monitor(application: AppHandle) {
    // Wait briefly on startup for Collapse issue-host backfill so pre-#612 projects do not
    // dispatch against GitHub while their import status is being verified (#613).
    crate::collapse_mirror::wait_for_issue_host_backfill(&application, Duration::from_secs(8));
    let mut readers = BTreeMap::new();
    let mut failed = BTreeMap::<u32, Instant>::new();
    let mut dependency_reads = BTreeMap::<u32, Instant>::new();
    loop {
        if !crate::rosen::paused(&application) {
            match projects(&application) {
                Ok(projects) => {
                    for project in &projects {
                        if failed
                            .get(&project.id)
                            .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
                        {
                            continue;
                        }
                        let result = pass(&application, project, &mut readers);
                        if let Err(error) = result {
                            failed.insert(project.id, Instant::now());
                            report(save_error(&application, project.id, error));
                        } else {
                            failed.remove(&project.id);
                        }
                    }
                    report(read_other_dependencies(
                        &application,
                        &projects,
                        &mut readers,
                        &mut dependency_reads,
                    ));
                }
                Err(error) => report::<()>(Err(error)),
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// Reads the dependencies of one project that dispatch passes over, when one is due (#510).
/// One per tick, so a long project list never holds up the next dispatch pass.
fn read_other_dependencies(
    application: &AppHandle,
    dispatched: &[ProjectItem],
    readers: &mut BTreeMap<u32, Reader>,
    read: &mut BTreeMap<u32, Instant>,
) -> Result<(), String> {
    let (existing, due) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let projects = &store.state().projects;
        let existing: HashSet<u32> = projects.iter().map(|project| project.id).collect();
        let due = projects
            .iter()
            .filter(|project| !dispatched.iter().any(|other| other.id == project.id))
            .find(|project| {
                read.get(&project.id)
                    .is_none_or(|time| time.elapsed() >= DEPENDENCY_REFRESH)
            })
            .cloned();
        (existing, due)
    };
    lock(
        application.state::<Dependencies>().inner(),
        "issue dependencies",
    )?
    .retain_sources(|project| existing.contains(&project));
    // A project that leaves dispatch is due at once: its backlog is no longer refreshed.
    read.retain(|project, _| {
        existing.contains(project) && !dispatched.iter().any(|other| other.id == *project)
    });
    let Some(project) = due else {
        return Ok(());
    };
    read.insert(project.id, Instant::now());
    // A project that cannot be read now keeps its last dependencies until the next attempt.
    let reader = reader(&project, readers);
    if let Ok(backlog) = read_issues(reader) {
        record_dependencies(application, project.id, &backlog, &BTreeMap::new())?;
        convert_dependency_holds(reader, &backlog);
        reader.backlog = backlog;
        lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
            .insert(project.id, reader.backlog.clone());
        holds::refresh(reader);
    }
    Ok(())
}

/// The sweep that retires `needs-owner` as a dependency hold (#511). An open issue whose
/// body still says `Held for dependencies: waits for #N …` gets GitHub links to those
/// issues, loses the label and has the line rewritten, so each note is converted once and
/// dispatch then starts the issue when its blockers close. It runs on issues already
/// read, a few per read so a long list never stalls dispatch. The converted issues keep
/// their label in `backlog` until the next read shows the links.
fn convert_dependency_holds(reader: &mut Reader, backlog: &Backlog) {
    let Some(client) = &reader.client else {
        return;
    };
    let held: Vec<u32> = backlog
        .issues
        .iter()
        .filter(|issue| IssueClient::has_dependency_hold(issue))
        .map(|issue| issue.number)
        .filter(|number| {
            reader
                .hold_retry
                .get(number)
                .is_none_or(|time| time.elapsed() >= HOLD_RETRY)
        })
        .take(HOLDS_PER_READ)
        .collect();
    for number in held {
        match client.convert_dependency_hold(number) {
            Ok(_) => {
                reader.hold_retry.remove(&number);
            }
            Err(error) => {
                // The issue stays held as it was; a blocker that is not an issue needs a person.
                reader.hold_retry.insert(number, Instant::now());
                report::<()>(Err(format!(
                    "Could not convert the dependency hold on {}#{number} into GitHub links: {error}",
                    backlog.repository
                )));
            }
        }
    }
}

fn record_dependencies(
    application: &AppHandle,
    project: u32,
    backlog: &Backlog,
    extra_open_blockers: &BTreeMap<u32, Vec<String>>,
) -> Result<(), String> {
    lock(
        application.state::<Dependencies>().inner(),
        "issue dependencies",
    )?
    .replace_source_with_blockers(
        project,
        &backlog.repository,
        &backlog.issues,
        extra_open_blockers,
    );
    Ok(())
}

fn projects(application: &AppHandle) -> Result<Vec<ProjectItem>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .state()
        .projects
        .iter()
        .filter(|project| {
            !project.continuous_dispatch.cleanup.is_empty()
                || !project.continuous_dispatch.no_change_reports.is_empty()
                || !project.continuous_dispatch.issue_overlaps.is_empty()
                || !project.continuous_dispatch.landed_holds.is_empty()
                || !project.continuous_dispatch.remote_work_holds.is_empty()
                || !project.continuous_dispatch.reopened_landed_holds.is_empty()
                || (project.version_control == VersionControl::Rosen
                    && (project.continuous_dispatch.slots > 0
                        || project
                            .threads
                            .iter()
                            .any(|thread| thread.rosen_change.is_some())
                        || project.threads.iter().any(|thread| {
                            thread.started_by_dispatch
                                && thread.state == velocity_store::ThreadState::Working
                        })
                        || store
                            .active_project()
                            .is_some_and(|active| active.id == project.id)))
        })
        .cloned()
        .collect())
}

fn pass(
    application: &AppHandle,
    project: &ProjectItem,
    readers: &mut BTreeMap<u32, Reader>,
) -> Result<(), String> {
    let reader = reader(project, readers);
    report_no_change_issues(application, project, reader)?;
    report_launch_failures(application, project, reader)?;
    cleanup_empty_threads(application, project)?;
    finish_empty_threads(application, project)?;
    if project_has_capacity(project) {
        // Local work takes precedence even while GitHub is slow or unavailable.
        start_ready(application, project.id, None)?;
    }
    refresh_backlog(application, project.id, reader)?;
    text_blockers::refresh(application, project, reader)?;
    holds::refresh(reader);
    reopened_land::refresh(application, project, &reader.backlog);
    reconcile_attempts(application, project.id, reader)?;
    let claimed = claimed_pieces(application, project.id).unwrap_or_default();
    restart_wait::refresh(application, project, &reader.backlog, &claimed);
    overlap::refresh(application, project.id, reader)?;
    parked::refresh(application, project, reader)?;
    start_ready(application, project.id, Some(reader))
}

/// Clears "already attempted at this version" records when all referenced blockers close
/// or after dispatch slots have been idle for 30 minutes without a landed change (#617, #618).
fn reconcile_attempts(
    application: &AppHandle,
    identifier: u32,
    reader: &mut Reader,
) -> Result<(), String> {
    let (all_projects, project) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let Some(project) = store.project(identifier).cloned() else {
            return Ok(());
        };
        (store.state().projects.clone(), project)
    };
    let now = unix_seconds();
    let repository = reader.backlog.repository.clone();
    let attempted_numbers = attempted_numbers_for_repo(&project, &repository);
    let slots_idle_secs = reconcile_idle_seconds(
        application,
        &project,
        &reader.backlog,
        &attempted_numbers,
        now,
    )?;
    if reader.backlog.error.is_some() || repository.is_empty() {
        return Ok(());
    }
    reader
        .attempt_comments
        .retain(|number, _| attempted_numbers.contains(number));
    if attempted_numbers.is_empty() {
        return Ok(());
    }
    let comments_by_issue = load_attempt_comments(reader, &attempted_numbers);
    let blocker_context =
        text_blockers::context_for_project(application, identifier, Some(&reader.backlog));
    let plan = attempts_to_clear_with_context(
        &all_projects,
        &project,
        &repository,
        &reader.backlog.issues,
        &comments_by_issue,
        slots_idle_secs,
        now,
        &blocker_context,
    );
    if plan.clear.is_empty() && plan.add_open_blockers.is_empty() {
        return Ok(());
    }
    let clear_numbers: Vec<u32> = plan.clear.iter().map(|(number, _)| *number).collect();
    let changed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .reconcile_dispatch_attempts(
        identifier,
        &repository,
        &clear_numbers,
        &plan.add_open_blockers,
    )
    .map_err(|error| error.to_string())?;
    if changed {
        record_cleared_attempts(&project.name, &plan.clear);
    }
    Ok(())
}

fn attempted_numbers_for_repo(project: &ProjectItem, repository: &str) -> HashSet<u32> {
    if repository.is_empty() {
        return HashSet::new();
    }
    project
        .continuous_dispatch
        .attempts
        .iter()
        .filter(|attempt| attempt.repository.eq_ignore_ascii_case(repository))
        .map(|attempt| attempt.number)
        .collect()
}

fn reconcile_idle_seconds(
    application: &AppHandle,
    project: &ProjectItem,
    backlog: &Backlog,
    attempted_numbers: &HashSet<u32>,
    now: u64,
) -> Result<Option<u64>, String> {
    let busy = busy_count(project);
    crate::fleet_ledger::observe_idle_slots(
        application,
        project.id,
        project.continuous_dispatch.slots,
        busy,
        project.continuous_dispatch.paused,
        now,
    );
    let slots_free_and_empty =
        project.continuous_dispatch.slots > 0 && !project.continuous_dispatch.paused && busy == 0;
    if !slots_free_and_empty {
        sync_idle_since(application, project, false, now)?;
        return Ok(None);
    }
    if backlog.repository.is_empty() {
        return Ok(project
            .continuous_dispatch
            .idle_since_secs
            .map(|since| now.saturating_sub(since)));
    }
    let has_idle_work = backlog.error.is_some()
        || !attempted_numbers.is_empty()
        || backlog_has_idle_work(project, &backlog.issues);
    Ok(sync_idle_since(application, project, has_idle_work, now)?
        .map(|since| now.saturating_sub(since)))
}

fn backlog_has_idle_work(project: &ProjectItem, issues: &[GitHubIssue]) -> bool {
    issues.iter().any(|issue| {
        issue.state == IssueState::Open
            && !velocity_core::issue_holds::has_label(
                &issue.labels,
                velocity_core::issue_holds::TRACKING_LABEL,
            )
    }) || project
        .tasks
        .iter()
        .any(|task| velocity_store::continuous_dispatch::available_task(project, task))
}

/// Persists or clears the project's wall-clock idle-spell start so restarting Velocity
/// during an idle spell preserves elapsed idle time for retries and project health (#618).
fn sync_idle_since(
    application: &AppHandle,
    project: &ProjectItem,
    idle_now: bool,
    now: u64,
) -> Result<Option<u64>, String> {
    let desired = if idle_now {
        Some(project.continuous_dispatch.idle_since_secs.unwrap_or(now))
    } else {
        None
    };
    if desired != project.continuous_dispatch.idle_since_secs {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .set_dispatch_idle_since(project.id, desired)
        .map_err(|error| error.to_string())?;
    }
    Ok(desired)
}

fn record_cleared_attempts(project_name: &str, cleared: &[(u32, AttemptClearReason)]) {
    for (number, reason) in cleared {
        match reason {
            AttemptClearReason::BlockerClosed(blocker) => {
                let noun = if blocker.contains(", ") {
                    "blockers"
                } else {
                    "blocker"
                };
                crate::diagnostics::record(
                    "continuous dispatch",
                    &format!(
                        "{project_name}: cleared attempt for #{number} ({noun} {blocker} closed)"
                    ),
                );
            }
            AttemptClearReason::IdleWithoutLand => crate::diagnostics::record(
                "continuous dispatch",
                &format!(
                    "{project_name}: cleared attempt for #{number} (slots idle 30 min without landed change)"
                ),
            ),
            AttemptClearReason::ClosedOrVersionChanged => {}
        }
    }
}

fn load_attempt_comments(
    reader: &mut Reader,
    attempted_numbers: &HashSet<u32>,
) -> BTreeMap<u32, Vec<String>> {
    let mut map = BTreeMap::new();
    let Some(client) = reader.client.as_ref() else {
        return map;
    };
    for issue in &reader.backlog.issues {
        if !attempted_numbers.contains(&issue.number) {
            continue;
        }
        let cached = reader
            .attempt_comments
            .get(&issue.number)
            .filter(|(updated_at, _)| updated_at == &issue.updated_at)
            .map(|(_, bodies)| bodies.clone());
        if let Some(bodies) = cached {
            map.insert(issue.number, bodies);
            continue;
        }
        if let Ok(bodies) = client.read_comment_bodies(issue.number) {
            reader
                .attempt_comments
                .insert(issue.number, (issue.updated_at.clone(), bodies.clone()));
            map.insert(issue.number, bodies);
        }
    }
    map
}

fn start_ready(
    application: &AppHandle,
    identifier: u32,
    mut reader: Option<&mut Reader>,
) -> Result<(), String> {
    if !has_capacity(application, identifier)? {
        return Ok(());
    }
    let defaults = Backlog::default();
    let backlog = reader.as_ref().map_or(&defaults, |reader| &reader.backlog);
    let blocker_context = text_blockers::context_for_project(
        application,
        identifier,
        reader.as_deref().map(|r| &r.backlog),
    );
    let mut claimed = claimed_pieces(application, identifier)?;
    let unblocks = unblock_counts(application, &backlog.repository)?;
    let work = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(identifier).ok_or("The project was removed")?;
        include_overlap_waits(project, &backlog.repository, &mut claimed);
        include_launch_holds(project, &backlog.repository, &mut claimed);
        include_remote_work_holds(project, &backlog.repository, &backlog.issues, &mut claimed);
        restart_wait::include_restart_waits(application, identifier, &mut claimed);
        ordered_work_with_blockers(
            &store.state().projects,
            project,
            &backlog.repository,
            &backlog.issues,
            &claimed,
            reader.is_some() && backlog.error.is_none(),
            &unblocks,
            &blocker_context,
        )
    };
    for item in work {
        if !has_capacity(application, identifier)? {
            break;
        }
        match item {
            DispatchWork::Task(task) => start_task(application, identifier, task.id)?,
            DispatchWork::Issue(issue, _) => {
                let reader = reader
                    .as_deref_mut()
                    .ok_or("The dispatch issue source was removed")?;
                try_start_issue(application, identifier, reader, issue.number)?;
            }
        }
    }
    Ok(())
}

fn try_start_issue(
    application: &AppHandle,
    identifier: u32,
    reader: &mut Reader,
    number: u32,
) -> Result<(), String> {
    let client = reader
        .client
        .as_ref()
        .ok_or("The dispatch issue source was removed")?;
    let repository = client.repository().to_owned();
    let state = application.state::<ManagedStateStore>();
    if lock(state.inner(), "persisted state")?
        .project(identifier)
        .is_some_and(|project| {
            project
                .continuous_dispatch
                .issue_overlaps
                .contains_key(&number)
        })
    {
        return Ok(());
    }
    // Re-read before claiming: the issue's text, eligibility or ownership may have changed.
    let fresh = client.get(number).map_err(|error| error.to_string())?;
    if let Some(issue) = reader
        .backlog
        .issues
        .iter_mut()
        .find(|issue| issue.number == number)
    {
        *issue = fresh.clone();
    }
    if fresh.state != IssueState::Open {
        return Ok(());
    }
    let fresh_project = lock(state.inner(), "persisted state")?
        .project(identifier)
        .cloned();
    if let Some(project) = fresh_project {
        if reopened_land::check_issue(application, &project, &repository, &fresh).is_some() {
            return Ok(());
        }
        let claimed = claimed_pieces(application, identifier).unwrap_or_default();
        if restart_wait::check_issue(application, &project, &repository, &fresh, &claimed).is_some()
        {
            return Ok(());
        }
    }
    if hold_external_issue(application, identifier, reader, &fresh)? {
        overlap::refresh(application, identifier, reader)?;
        return Ok(());
    }
    overlap::refresh(application, identifier, reader)?;
    if !reader
        .backlog
        .issues
        .iter()
        .any(|issue| issue.number == number)
    {
        return Ok(());
    }
    let open_numbers: HashSet<u32> = reader
        .backlog
        .issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open)
        .map(|issue| issue.number)
        .collect();
    let comments =
        text_blockers::load_issue_comments(reader, &fresh).map_err(|error| error.to_string())?;
    let current_project = lock(state.inner(), "persisted state")?
        .project(identifier)
        .cloned();
    if let Some(project) = current_project {
        text_blockers::refresh_for_issue(application, &project, reader, &fresh, &comments)?;
    }
    let blocker_context =
        text_blockers::context_for_project(application, identifier, Some(&reader.backlog));
    start_issue(
        application,
        identifier,
        &repository,
        &fresh,
        &open_numbers,
        &comments,
        &blocker_context,
    )?;
    overlap::refresh(application, identifier, reader)
}

/// Velocity's comments outlive local thread history and cross machines (#504, #519).
/// `true` when the issue already landed on the mirror or another machine is actively
/// working on it, so this machine must not open a Rosen change for it.
fn hold_external_issue(
    application: &AppHandle,
    identifier: u32,
    reader: &Reader,
    issue: &GitHubIssue,
) -> Result<bool, String> {
    let client = reader
        .client
        .as_ref()
        .ok_or("The dispatch issue source was removed")?;
    let now = unix_seconds();
    let identity = ThreadIssue::new(client.repository().into(), issue.number)
        .with_identity(issue.title.clone(), issue.host);
    match client
        .external_issue_state(issue.number, now)
        .map_err(|error| error.to_string())?
    {
        Some(ExternalIssueState::Landed { commit, landed_at }) => {
            if reopened_after(issue, landed_at) {
                clear_stale_external_holds(application, identifier, issue.number)?;
                return Ok(false);
            }
            handle_external_land(application, identifier, client, issue, commit, landed_at)
        }
        Some(ExternalIssueState::InProgressElsewhere { started_at }) => {
            if reopened_after(issue, Some(started_at)) {
                clear_stale_external_holds(application, identifier, issue.number)?;
                return Ok(false);
            }
            let hold = RemoteWorkHold::new(
                identity,
                issue.updated_at.clone(),
                started_at,
                started_at.saturating_add(REMOTE_PROGRESS_WINDOW_SECONDS),
            );
            lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?
            .save_remote_work_hold(identifier, hold)
            .map_err(|error| error.to_string())?;
            Ok(true)
        }
        None => {
            clear_stale_external_holds(application, identifier, issue.number)?;
            Ok(false)
        }
    }
}

/// Whether GitHub recorded a reopen event strictly after an earlier external land or run (#602).
fn reopened_after(issue: &GitHubIssue, event_seconds: Option<u64>) -> bool {
    let (Some(reopened), Some(event)) = (
        issue
            .reopened_at
            .as_deref()
            .and_then(velocity_store::rosen_work::parse_timestamp_seconds),
        event_seconds,
    ) else {
        return false;
    };
    reopened > event
}

fn handle_external_land(
    application: &AppHandle,
    identifier: u32,
    client: &IssueClient,
    issue: &GitHubIssue,
    commit: String,
    landed_at: Option<u64>,
) -> Result<bool, String> {
    let now = unix_seconds();
    let identity = ThreadIssue::new(client.repository().into(), issue.number)
        .with_identity(issue.title.clone(), issue.host);
    if commit_in_mirror_or_local(application, identifier, client, &commit)? {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .save_landed_hold(
            identifier,
            identity,
            commit,
            issue.updated_at.clone(),
            landed_at.unwrap_or(now),
        )
        .map_err(|error| error.to_string())?;
        crate::rosen_landing::wake(application);
        return Ok(true);
    }
    if let Some(at) = landed_at
        && now.saturating_sub(at) < MIRROR_LAG_HOLD_SECONDS
    {
        let hold = RemoteWorkHold::new(
            identity,
            issue.updated_at.clone(),
            at,
            at.saturating_add(MIRROR_LAG_HOLD_SECONDS),
        );
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .save_remote_work_hold(identifier, hold)
        .map_err(|error| error.to_string())?;
        return Ok(true);
    }
    clear_stale_external_holds(application, identifier, issue.number)?;
    Ok(false)
}

fn commit_in_mirror_or_local(
    application: &AppHandle,
    identifier: u32,
    client: &IssueClient,
    commit: &str,
) -> Result<bool, String> {
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(false);
    }
    if client
        .rosen_commit_mirrored(commit)
        .map_err(|error| error.to_string())?
    {
        return Ok(true);
    }
    Ok(local_base_contains(application, identifier, commit))
}

fn local_base_contains(application: &AppHandle, project: u32, commit: &str) -> bool {
    let Ok(data) = application.path().app_data_dir() else {
        return false;
    };
    let server = velocity_rosen::ProjectServer::new(&data, project);
    matches!(
        tauri::async_runtime::block_on(velocity_rosen::post_land::base_contains_commit(
            &server, commit
        )),
        Ok(true)
    )
}

fn clear_stale_external_holds(
    application: &AppHandle,
    identifier: u32,
    number: u32,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    store
        .clear_remote_work_hold(identifier, number)
        .map_err(|error| error.to_string())?;
    store
        .remove_landed_hold(identifier, number)
        .map_err(|error| error.to_string())
}

fn include_remote_work_holds(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &mut HashSet<String>,
) {
    let now = unix_seconds();
    for issue in issues {
        let identity = ThreadIssue::new(repository.into(), issue.number);
        if project
            .remote_work_hold(&identity, Some(&issue.updated_at), now)
            .is_some()
        {
            claimed.insert(format!("issue:{}", issue.number));
        }
    }
}

fn include_overlap_waits(project: &ProjectItem, repository: &str, claimed: &mut HashSet<String>) {
    claimed.extend(
        project
            .continuous_dispatch
            .issue_overlaps
            .values()
            .filter(|wait| wait.issue.repository.eq_ignore_ascii_case(repository))
            .map(|wait| format!("issue:{}", wait.issue.number)),
    );
}

/// Issues whose threads failed before starting too often in a row wait out their hold
/// like claimed work (#514).
fn include_launch_holds(project: &ProjectItem, repository: &str, claimed: &mut HashSet<String>) {
    claimed.extend(
        project
            .continuous_dispatch
            .launch_holds(unix_seconds())
            .filter(|issue| issue.repository.eq_ignore_ascii_case(repository))
            .map(|issue| format!("issue:{}", issue.number)),
    );
}

/// A dispatch thread failed before its agent's session began (#514). It did no work, so
/// its issue goes back to dispatch now and the failure does not count as an attempt.
/// The next pass posts the reason on the issue. Other threads are left as they are.
pub(crate) fn return_unstarted(
    application: &AppHandle,
    key: velocity_threads::ThreadKey,
    headline: &str,
    detail: &str,
) {
    // The detail can carry the agent's last stderr lines; the comment needs the cause.
    let detail: String = detail.chars().take(1500).collect();
    let reason = format!("{headline}\n\n{}", detail.trim());
    let returned = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .and_then(|mut store| {
        store
            .return_unstarted_dispatch(key.0, key.1, &reason, unix_seconds())
            .map_err(|error| error.to_string())
    });
    match returned {
        // The claim on the issue would keep it out of dispatch as long as the attempt did.
        Ok(true) => report(crate::airc_claims::release(application, key)),
        Ok(false) => {}
        Err(error) => report::<()>(Err(error)),
    }
}

fn report_launch_failures(
    application: &AppHandle,
    project: &ProjectItem,
    reader: &mut Reader,
) -> Result<(), String> {
    for (&thread, pending) in &project.continuous_dispatch.launch_failure_reports {
        if project.has_host_issue_problem(reader.host, pending.issue.number) {
            continue;
        }
        if reader
            .report_retry
            .get(&thread)
            .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
        {
            continue;
        }
        let byline = velocity_threads::issue_activity::issue_byline(project.id, Some(thread), None);
        let attempt = format!("{}-{thread}-{}", project.id, pending.failure);
        let result = IssueClient::for_host(
            Path::new(&reader.directory),
            reader.host,
            &pending.issue.repository,
        )
        .and_then(|client| {
            client.post_launch_failure(
                pending.issue.number,
                &attempt,
                &byline,
                &pending.reason,
                &launch_failure_outlook(pending.failure),
            )
        });
        if let Err(error) = result {
            if error.is_not_found() {
                tauri::async_runtime::block_on(crate::rosen_issue::record_missing_issue_once(
                    application,
                    project.id,
                    Path::new(&reader.directory),
                    reader.host,
                    &pending.issue.repository,
                    pending.issue.number,
                    error.to_string(),
                ));
                reader.report_retry.remove(&thread);
                continue;
            }
            reader.report_retry.insert(thread, Instant::now());
            report::<()>(Err(format!(
                "Could not report the failed start on {}#{}: {error}",
                pending.issue.repository, pending.issue.number
            )));
            continue;
        }
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_launch_failure_report(project.id, thread)
        .map_err(|error| error.to_string())?;
        reader.report_retry.remove(&thread);
    }
    Ok(())
}

/// What the issue's comment says happens after failed start number `failure` in a row.
fn launch_failure_outlook(failure: u32) -> String {
    let returned = "The agent ran no turn, so this does not count as an attempt. \
                    The issue is back in continuous dispatch.";
    if failure < velocity_store::continuous_dispatch::UNSTARTED_RETURNS {
        return format!("{returned} It starts again on the next pass.");
    }
    format!(
        "{returned} This was failed start {failure} in a row. If the next one fails too, \
         dispatch waits an hour before each further try, doubling up to a day, and posts \
         no more comments until a thread starts."
    )
}

fn refresh_backlog(
    application: &AppHandle,
    project: u32,
    reader: &mut Reader,
) -> Result<(), String> {
    let interval = match reader.host {
        IssueHost::Collapse => COLLAPSE_CURSOR_REFRESH,
        IssueHost::GitHub => GITHUB_BACKLOG_REFRESH,
    };
    let force_refresh = application
        .try_state::<ForceBacklogRefresh>()
        .and_then(|forced| lock(forced.inner(), "backlog refresh").ok())
        .is_some_and(|mut forced| forced.remove(&project));
    if force_refresh
        || reader
            .refreshed
            .is_none_or(|time| time.elapsed() >= interval)
    {
        // Failed reads also back off: an unavailable API must not be hammered every tick.
        reader.refreshed = Some(Instant::now());
        match read_issues(reader) {
            Ok(mut backlog) => {
                convert_dependency_holds(reader, &backlog);
                backlog.comments = std::mem::take(&mut reader.backlog.comments);
                reader.backlog = backlog;
                // A failed read keeps the last dependencies, so the order holds through it.
                record_dependencies(application, project, &reader.backlog, &BTreeMap::new())?;
                prune_landed_holds(application, project, &reader.backlog)?;
            }
            Err(error) => {
                reader.backlog.issues.clear();
                reader.backlog.error = Some(error.to_string());
            }
        }
        lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
            .insert(project, reader.backlog.clone());
    }
    Ok(())
}

/// A closed issue no longer waits; its hold goes with it.
fn prune_landed_holds(
    application: &AppHandle,
    project: u32,
    backlog: &Backlog,
) -> Result<(), String> {
    if backlog.repository.is_empty() {
        return Ok(());
    }
    let open: HashSet<u32> = backlog.issues.iter().map(|issue| issue.number).collect();
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    store
        .prune_landed_holds(project, &backlog.repository, |number| {
            open.contains(&number)
        })
        .map_err(|error| error.to_string())?;
    store
        .prune_remote_work_holds(project, &backlog.repository, |number| {
            open.contains(&number)
        })
        .map_err(|error| error.to_string())?;
    // The same read ends the count of failed starts for closed and started issues (#514).
    store
        .prune_unstarted_failures(project, &backlog.repository, |number| {
            open.contains(&number)
        })
        .map_err(|error| error.to_string())
}

fn reader<'a>(project: &ProjectItem, readers: &'a mut BTreeMap<u32, Reader>) -> &'a mut Reader {
    let host = project.effective_issue_host();
    let remote = project.issue_host_repository().map(|(_, repo)| repo);
    let setup = || Reader {
        directory: project.repository_path.clone(),
        host,
        remote: remote.clone(),
        client: None,
        refreshed: None,
        report_retry: BTreeMap::new(),
        hold_retry: BTreeMap::new(),
        attempt_comments: BTreeMap::new(),
        backlog: Backlog::default(),
        parked: parked::Parked::new(),
        holds: holds::Holds::new(),
        text_blockers: text_blockers::TextBlockers::new(),
    };
    let reader = readers.entry(project.id).or_insert_with(setup);
    if reader.directory != project.repository_path || reader.host != host || reader.remote != remote
    {
        *reader = setup();
    }
    reader
}

fn read_issues(reader: &mut Reader) -> Result<Backlog, velocity_github::GitHubCommandError> {
    let directory = Path::new(&reader.directory);
    if reader.host == IssueHost::GitHub
        && reader.remote.is_none()
        && reader
            .client
            .as_ref()
            .is_some_and(|client| !client.is_current(directory))
    {
        reader.client = None;
    }
    if reader.client.is_none() {
        reader.client = match (reader.host, reader.remote.as_deref()) {
            (host, Some(remote)) => Some(IssueClient::for_host(directory, host, remote)?),
            (IssueHost::GitHub, None)
                if directory.join(".git").exists() && IssueClient::has_origin(directory)? =>
            {
                Some(IssueClient::discover(directory)?)
            }
            _ => None,
        };
    }
    let Some(client) = &reader.client else {
        return Ok(Backlog::default());
    };
    Ok(Backlog {
        repository: client.repository().into(),
        issues: client.open_issues()?,
        comments: BTreeMap::new(),
        error: None,
        refreshed_at: Some(Instant::now()),
    })
}

fn has_capacity(application: &AppHandle, identifier: u32) -> Result<bool, String> {
    if velocity_rosen::memory::is_low() {
        return Ok(false);
    }
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store.project(identifier).is_some_and(project_has_capacity))
}

fn start_issue(
    application: &AppHandle,
    identifier: u32,
    repository: &str,
    issue: &GitHubIssue,
    open_numbers: &HashSet<u32>,
    cached_comments: &[String],
    blocker_context: &TextBlockerContext,
) -> Result<(), String> {
    if velocity_rosen::memory::is_low() {
        return Ok(());
    }
    let claimed = claimed_pieces(application, identifier)?;
    let (plan, thread, rosen_project) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(identifier).ok_or("The project was removed")?;
        if !project_has_capacity(project) || has_user_task(project, &claimed) {
            return Ok(());
        }
        if !dispatchable_issue_with_blockers(
            &store.state().projects,
            project,
            repository,
            issue,
            &claimed,
            blocker_context,
        ) || restart_wait::is_waiting(application, identifier, issue.number)
        {
            return Ok(());
        }
        if project
            .remote_work_hold(
                &ThreadIssue::new(repository.into(), issue.number),
                Some(&issue.updated_at),
                unix_seconds(),
            )
            .is_some()
        {
            return Ok(());
        }
        if project
            .continuous_dispatch
            .issue_overlaps
            .get(&issue.number)
            .is_some_and(|wait| wait.issue.repository.eq_ignore_ascii_case(repository))
        {
            return Ok(());
        }
        let open_referenced_blockers = open_referenced_blockers_for_issue_with_context(
            &store.state().projects,
            project,
            repository,
            issue,
            open_numbers,
            cached_comments,
            blocker_context,
        );
        let rosen_project =
            (project.version_control == VersionControl::Rosen).then(|| project.clone());
        let plan = velocity_threads::dispatch::plan_project_issue_dispatch(
            &store, identifier, repository, issue, false,
        )
        .map_err(|error| error.to_string())?;
        let thread = store
            .record_continuous_dispatch_with_blockers(
                identifier,
                plan.launch.thread.clone(),
                plan.issue,
                issue.updated_at.clone(),
                issue.reopened_at.as_deref(),
                open_referenced_blockers,
                unix_seconds(),
            )
            .map_err(|error| error.to_string())?;
        (plan.launch, thread, rosen_project)
    };
    let provider_name = plan.thread.provider.key();
    let model_name = plan.thread.model_name.clone();
    if let Some(project) = rosen_project {
        crate::graph_access::warn_dispatch_signatures(
            application,
            &project,
            issue.number,
            &[issue.title.clone(), issue.body.clone()],
        );
    }
    crate::fleet_ledger::record_dispatch(
        application,
        identifier,
        repository,
        issue,
        thread,
        &provider_name,
        &model_name,
    );
    crate::threads::start_planned_with_selection(application, plan, thread, false)?;
    record_dispatched(application, identifier);
    Ok(())
}

fn start_task(application: &AppHandle, identifier: u32, task: u32) -> Result<(), String> {
    let claimed = claimed_pieces(application, identifier)?;
    let (plan, thread) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(identifier).ok_or("The project was removed")?;
        let Some(task) = project
            .tasks
            .iter()
            .find(|entry| entry.id == task && dispatchable_task(project, entry, &claimed))
            .filter(|_| project_has_capacity(project))
        else {
            return Ok(());
        };
        if task.added_by != TaskAttribution::User && has_user_task(project, &claimed) {
            return Ok(());
        }
        let task_identifier = task.id;
        let plan = plan_task(&store, project, task).map_err(|error| error.to_string())?;
        let thread = store
            .record_continuous_task(identifier, plan.thread.clone(), task_identifier)
            .map_err(|error| error.to_string())?;
        (plan, thread)
    };
    crate::threads::start_planned_with_selection(application, plan, thread, false)?;
    record_dispatched(application, identifier);
    Ok(())
}

fn has_user_task(project: &ProjectItem, claimed: &HashSet<String>) -> bool {
    project.tasks.iter().any(|task| {
        task.added_by == TaskAttribution::User && dispatchable_task(project, task, claimed)
    })
}

fn claimed_pieces(application: &AppHandle, identifier: u32) -> Result<HashSet<String>, String> {
    Ok(dispatch_claims(application, identifier)?
        .iter()
        .map(|claim| claim.key().to_owned())
        .collect())
}

fn dispatch_claims(
    application: &AppHandle,
    identifier: u32,
) -> Result<Vec<velocity_airc::Claim>, String> {
    crate::airc_claims::live_dispatch_claims(application, identifier).map(|(claims, _)| claims)
}

fn save_error(application: &AppHandle, identifier: u32, error: String) -> Result<(), String> {
    let mut backlogs = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?;
    backlogs.entry(identifier).or_default().error = Some(error);
    Ok(())
}

fn finish_empty_threads(application: &AppHandle, project: &ProjectItem) -> Result<(), String> {
    for thread in project.threads.iter().filter(|thread| {
        can_finish_empty_dispatch(thread)
            && (project.continuous_dispatch.slots > 0 || thread.started_by_dispatch)
            && thread.supervision.turns > 0
            && thread.rosen_change.is_some()
    }) {
        finish_empty_thread(application, project.id, thread)?;
    }
    Ok(())
}

fn finish_empty_thread(
    application: &AppHandle,
    project: u32,
    thread: &velocity_store::ThreadItem,
) -> Result<(), String> {
    let key = (project, thread.id);
    let lease = crate::threads::action_lock(application, key)?;
    let Ok(_guard) = lease.try_lock() else {
        return Ok(());
    };
    // The project snapshot preceded the lease; recovery or a send may have changed it.
    let Some(thread) = current_empty_thread(application, key)? else {
        return Ok(());
    };
    if !lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .is_at_step_boundary(key)
    {
        return Ok(());
    }
    let Some(change) = thread.rosen_change.as_deref() else {
        return Ok(());
    };
    let Some(root) = thread.worktree_path.as_deref() else {
        return Ok(());
    };
    if crate::parking::sync_active_rosen_waits(application, key, change)? {
        return Ok(());
    }
    let exhausted = idle_nudge_exhausted(&thread);
    let disposition = inspect_idle_change(application, project, change, Path::new(root))?;
    match (disposition, exhausted) {
        (
            velocity_rosen::dispatch::IdleChangeDisposition::PassingWork {
                head,
                changed_files,
                preflight_passed,
            },
            true,
        ) => finish_kept_work_thread(
            application,
            key,
            change,
            Path::new(root),
            &head,
            changed_files,
            preflight_passed,
        ),
        (velocity_rosen::dispatch::IdleChangeDisposition::Empty, _)
        | (velocity_rosen::dispatch::IdleChangeDisposition::UnverifiedEdits { .. }, true) => {
            finish_no_change_thread(application, key, &thread)
        }
        _ => Ok(()),
    }
}

fn finish_no_change_thread(
    application: &AppHandle,
    key: velocity_threads::ThreadKey,
    thread: &velocity_store::ThreadItem,
) -> Result<(), String> {
    if !crate::threads::stop_idle_thread(application, key)? {
        return Ok(());
    }
    let nick = if thread.started_by_dispatch && thread.issue.is_some() {
        dispatch_claims(application, key.0)?
            .into_iter()
            .find(|claim| thread.matches_airc_member(&claim.member))
            .map(|claim| claim.nick)
    } else {
        None
    };
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .finish_empty_dispatch_with_nick(key.0, thread.id, nick)
    .map_err(|error| error.to_string())?;
    // Release the AIRC claim now rather than waiting for the next pass's cleanup (#656).
    // `cleanup_empty_threads` retries the release if this call fails.
    report(crate::airc_claims::release(application, key));
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

/// Stops an idle-nudge-exhausted thread whose Rosen change has passing work while keeping
/// the change and workspace intact for handoff through `previous_work_thread` (#565).
fn finish_kept_work_thread(
    application: &AppHandle,
    key: velocity_threads::ThreadKey,
    change: &str,
    root: &Path,
    head: &str,
    changed_files: usize,
    preflight_passed: bool,
) -> Result<(), String> {
    if !crate::threads::stop_idle_thread(application, key)? {
        return Ok(());
    }
    tauri::async_runtime::block_on(crate::rosen::stop_watch(application, root));
    crate::airc_claims::release(application, key)?;
    let short_change = change.get(..8).unwrap_or(change);
    let short_head = head.get(..8).unwrap_or(head);
    let status = if preflight_passed {
        "preflight passed"
    } else {
        "non-overlap preflight checks passed"
    };
    let reason = format!(
        "Stopped after idle supervision nudge; kept Rosen change {short_change} (head \
         {short_head}, {changed_files} changed file(s), {status}) and workspace for handoff."
    );
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .finish_kept_work_dispatch(key.0, key.1, reason)
    .map_err(|error| error.to_string())?;
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

fn inspect_idle_change(
    application: &AppHandle,
    project: u32,
    change: &str,
    root: &Path,
) -> Result<velocity_rosen::dispatch::IdleChangeDisposition, String> {
    tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, project).await?;
        velocity_rosen::dispatch::inspect_idle_change(&server, change, root)
            .await
            .map_err(|error| error.to_string())
    })
}

fn report_no_change_issues(
    application: &AppHandle,
    project: &ProjectItem,
    reader: &mut Reader,
) -> Result<(), String> {
    for (&thread, pending) in &project.continuous_dispatch.no_change_reports {
        if project.has_host_issue_problem(reader.host, pending.issue.number) {
            continue;
        }
        if reader
            .report_retry
            .get(&thread)
            .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
        {
            continue;
        }
        let byline = velocity_threads::issue_activity::issue_byline(
            project.id,
            Some(thread),
            pending.nick.as_deref(),
        );
        let all_projects = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .map(|store| store.state().projects.clone())
        .unwrap_or_default();
        let blocker_context =
            text_blockers::context_for_project(application, project.id, Some(&reader.backlog));
        let result = IssueClient::for_host(
            Path::new(&reader.directory),
            reader.host,
            &pending.issue.repository,
        )
        .and_then(|client| {
            client.post_no_change_with_blocker_check(
                pending.issue.number,
                &pending.change,
                &byline,
                &pending.reason,
                |issue| {
                    has_unsatisfied_blockers_with_context(
                        &all_projects,
                        project,
                        &pending.issue.repository,
                        issue,
                        &blocker_context,
                    ) || restart_wait::is_waiting(application, project.id, issue.number)
                },
            )
        });
        if let Err(error) = result {
            if error.is_not_found() {
                tauri::async_runtime::block_on(crate::rosen_issue::record_missing_issue_once(
                    application,
                    project.id,
                    Path::new(&reader.directory),
                    reader.host,
                    &pending.issue.repository,
                    pending.issue.number,
                    error.to_string(),
                ));
                reader.report_retry.remove(&thread);
                continue;
            }
            reader.report_retry.insert(thread, Instant::now());
            report::<()>(Err(format!(
                "Could not report no change on {}#{}: {error}",
                pending.issue.repository, pending.issue.number
            )));
            continue;
        }
        // The label now shows in the backlog, where the bounded review starts (#506).
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_no_change_report(project.id, thread)
        .map_err(|error| error.to_string())?;
        reader.report_retry.remove(&thread);
        reader.refreshed = None;
    }
    Ok(())
}

fn current_empty_thread(
    application: &AppHandle,
    key: velocity_threads::ThreadKey,
) -> Result<Option<velocity_store::ThreadItem>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .filter(|thread| can_finish_empty_dispatch(thread))
        .cloned())
}

fn cleanup_empty_threads(application: &AppHandle, project: &ProjectItem) -> Result<(), String> {
    for identifier in &project.continuous_dispatch.cleanup {
        let Some(thread) = project
            .threads
            .iter()
            .find(|thread| thread.id == *identifier)
        else {
            lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?
            .finish_dispatch_cleanup(project.id, *identifier)
            .map_err(|error| error.to_string())?;
            continue;
        };
        let Some(change) = thread.rosen_change.as_deref() else {
            continue;
        };
        // A thread that runs again, or is still starting, keeps its change until it is
        // idle: cleanup never abandons work under a live agent or a launch (#514).
        if !lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?
        .is_at_step_boundary((project.id, *identifier))
        {
            continue;
        }
        tauri::async_runtime::block_on(async {
            if let Some(root) = thread.worktree_path.as_deref() {
                crate::rosen::stop_watch(application, Path::new(root)).await;
            }
            let server = crate::rosen::server(application, project.id).await?;
            velocity_rosen::dispatch::abandon_finished(&server, change)
                .await
                .map_err(|error| error.to_string())
        })?;
        crate::airc_claims::release(application, (project.id, *identifier))?;
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_dispatch_cleanup(project.id, *identifier)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}
