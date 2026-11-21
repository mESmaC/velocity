//! Native background adapter for F8 continuous dispatch. It never uses the selected
//! project's issue rail, so changing projects cannot redirect a background launch.
mod holds;
pub(crate) mod overlap;
mod parked;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use velocity_github::{
    DependencyGraph, ExternalIssueState, GitHubIssue, IssueClient, IssueState,
    REMOTE_PROGRESS_WINDOW_SECONDS,
};
use velocity_store::continuous_dispatch::has_capacity as project_has_capacity;
use velocity_store::continuous_dispatch::{
    RemoteWorkHold, busy_count, can_finish_empty_dispatch, idle_nudge_exhausted,
};
use velocity_store::issues::ThreadIssue;
use velocity_store::{ProjectItem, TaskAttribution, VersionControl};
use velocity_threads::continuous_dispatch::{
    DispatchView, DispatchWork, SkippedIssue, dispatchable_issue, dispatchable_task,
    integration_notice, issue_skip_reason, ordered_work, plan_task, stalled_landed_issues,
    uncovered_base_notice,
};

#[derive(Clone, Default)]
struct Backlog {
    repository: String,
    issues: Vec<GitHubIssue>,
    error: Option<String>,
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
    crate::layout::off_ui_thread(move || view(&application, project_identifier)).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_continuous_dispatch(
    application: AppHandle,
    project_identifier: u32,
    slots: u32,
    paused: bool,
) -> Result<DispatchView, String> {
    crate::layout::off_ui_thread(move || {
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
    let mut claimed = claimed_pieces(application, identifier)?;
    let unblocks = unblock_counts(application, &backlog.repository)?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store.project(identifier).ok_or("The project was removed")?;
    include_overlap_waits(project, &backlog.repository, &mut claimed);
    include_remote_work_holds(project, &backlog.repository, &backlog.issues, &mut claimed);
    let work = ordered_work(
        project,
        &backlog.repository,
        &backlog.issues,
        &claimed,
        issues_known,
        &unblocks,
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
    let mut claimed = claimed_pieces(application, identifier)?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(project) = store.project(identifier) else {
        return Ok(false);
    };
    include_remote_work_holds(project, &backlog.repository, &backlog.issues, &mut claimed);
    Ok(busy_count(project) == 0
        && ordered_work(
            project,
            &backlog.repository,
            &backlog.issues,
            &claimed,
            true,
            // Only emptiness matters here, not the order.
            &BTreeMap::new(),
        )
        .is_empty())
}

/// When continuous dispatch last started a task or issue for each project, in Unix seconds (#509).
type LastDispatched = Mutex<BTreeMap<u32, u64>>;

/// Project-level continuous dispatch state for project health checks (#509).
#[derive(Clone, Debug, Default)]
pub(crate) struct DispatchHealthSnapshot {
    pub backlog_error: Option<String>,
    pub open_issues: usize,
    pub ready_count: usize,
    pub skipped: Vec<SkippedIssue>,
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
    let mut claimed = claimed_pieces(application, identifier).unwrap_or_default();
    let unblocks = unblock_counts(application, &backlog.repository).unwrap_or_default();
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let project = store.project(identifier)?;
    let issues_known = backlog.error.is_none();
    let stalled_landed = stalled_landed_issues(project, &backlog.repository, &backlog.issues, now);
    let skipped = if issues_known {
        backlog
            .issues
            .iter()
            .filter_map(|issue| {
                issue_skip_reason(project, &backlog.repository, issue, &claimed, now)
            })
            .collect()
    } else {
        Vec::new()
    };
    include_overlap_waits(project, &backlog.repository, &mut claimed);
    include_launch_holds(project, &backlog.repository, &mut claimed);
    include_remote_work_holds(project, &backlog.repository, &backlog.issues, &mut claimed);
    let ready_count = ordered_work(
        project,
        &backlog.repository,
        &backlog.issues,
        &claimed,
        issues_known,
        &unblocks,
    )
    .len();
    Some(DispatchHealthSnapshot {
        backlog_error: backlog.error,
        open_issues: backlog.issues.len(),
        ready_count,
        skipped,
        stalled_landed,
        last_dispatched_secs,
    })
}

fn record_dispatched(application: &AppHandle, identifier: u32) {
    if let Some(times) = application.try_state::<LastDispatched>()
        && let Ok(mut times) = lock(times.inner(), "last dispatched")
    {
        times.insert(identifier, unix_seconds());
    }
}

pub fn initialize(application: &AppHandle) {
    application.manage(Backlogs::default());
    application.manage(Dependencies::default());
    application.manage(IntegrationRecords::default());
    application.manage(LastDispatched::default());
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

struct Reader {
    directory: String,
    remote: Option<String>,
    client: Option<IssueClient>,
    refreshed: Option<Instant>,
    report_retry: BTreeMap<u32, Instant>,
    /// Issues whose retired dependency hold could not be converted, and when that failed.
    hold_retry: BTreeMap<u32, Instant>,
    backlog: Backlog,
    parked: parked::Parked,
    holds: holds::Holds,
}

fn monitor(application: AppHandle) {
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
        record_dependencies(application, project.id, &backlog)?;
        convert_dependency_holds(reader, &backlog);
        reader.backlog = backlog;
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
) -> Result<(), String> {
    lock(
        application.state::<Dependencies>().inner(),
        "issue dependencies",
    )?
    .replace_source(project, &backlog.repository, &backlog.issues);
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
    holds::refresh(reader);
    overlap::refresh(application, project.id, reader)?;
    parked::refresh(application, project, reader)?;
    start_ready(application, project.id, Some(reader))
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
        ordered_work(
            project,
            &backlog.repository,
            &backlog.issues,
            &claimed,
            reader.is_some() && backlog.error.is_none(),
            &unblocks,
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
    start_issue(application, identifier, &repository, &fresh)?;
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
    let identity = ThreadIssue::new(client.repository().into(), issue.number);
    match client
        .external_issue_state(issue.number, now)
        .map_err(|error| error.to_string())?
    {
        Some(ExternalIssueState::Landed { commit, landed_at }) => {
            handle_external_land(application, identifier, client, issue, commit, landed_at)
        }
        Some(ExternalIssueState::InProgressElsewhere { started_at }) => {
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

fn handle_external_land(
    application: &AppHandle,
    identifier: u32,
    client: &IssueClient,
    issue: &GitHubIssue,
    commit: String,
    landed_at: Option<u64>,
) -> Result<bool, String> {
    let now = unix_seconds();
    let identity = ThreadIssue::new(client.repository().into(), issue.number);
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
        if reader
            .report_retry
            .get(&thread)
            .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
        {
            continue;
        }
        let byline = velocity_threads::issue_activity::issue_byline(project.id, Some(thread), None);
        let attempt = format!("{}-{thread}-{}", project.id, pending.failure);
        let result =
            IssueClient::for_repository(Path::new(&reader.directory), &pending.issue.repository)
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
    if reader
        .refreshed
        .is_none_or(|time| time.elapsed() >= Duration::from_secs(30))
    {
        // Failed reads also back off: an unavailable API must not be hammered every tick.
        reader.refreshed = Some(Instant::now());
        match read_issues(reader) {
            Ok(backlog) => {
                convert_dependency_holds(reader, &backlog);
                reader.backlog = backlog;
                // A failed read keeps the last dependencies, so the order holds through it.
                record_dependencies(application, project, &reader.backlog)?;
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
    let setup = || Reader {
        directory: project.repository_path.clone(),
        remote: project.github_remote.clone(),
        client: None,
        refreshed: None,
        report_retry: BTreeMap::new(),
        hold_retry: BTreeMap::new(),
        backlog: Backlog::default(),
        parked: parked::Parked::new(),
        holds: holds::Holds::new(),
    };
    let reader = readers.entry(project.id).or_insert_with(setup);
    if reader.directory != project.repository_path || reader.remote != project.github_remote {
        *reader = setup();
    }
    reader
}

fn read_issues(reader: &mut Reader) -> Result<Backlog, velocity_github::GitHubCommandError> {
    let directory = Path::new(&reader.directory);
    if reader.remote.is_none()
        && reader
            .client
            .as_ref()
            .is_some_and(|client| !client.is_current(directory))
    {
        reader.client = None;
    }
    if reader.client.is_none() {
        reader.client = match reader.remote.as_deref() {
            Some(remote) => Some(IssueClient::for_repository(directory, remote)?),
            None if directory.join(".git").exists() && IssueClient::has_origin(directory)? => {
                Some(IssueClient::discover(directory)?)
            }
            None => None,
        };
    }
    let Some(client) = &reader.client else {
        return Ok(Backlog::default());
    };
    Ok(Backlog {
        repository: client.repository().into(),
        issues: client.open_issues()?,
        error: None,
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
        if !dispatchable_issue(project, repository, issue, &claimed) {
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
        let rosen_project =
            (project.version_control == VersionControl::Rosen).then(|| project.clone());
        let plan = velocity_threads::dispatch::plan_project_issue_dispatch(
            &store, identifier, repository, issue, false,
        )
        .map_err(|error| error.to_string())?;
        let thread = store
            .record_continuous_dispatch(
                identifier,
                plan.launch.thread.clone(),
                plan.issue,
                issue.updated_at.clone(),
            )
            .map_err(|error| error.to_string())?;
        (plan.launch, thread, rosen_project)
    };
    if let Some(project) = rosen_project {
        crate::graph_access::warn_dispatch_signatures(
            application,
            &project,
            issue.number,
            &[issue.title.clone(), issue.body.clone()],
        );
    }
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
    let channel = crate::airc_access::open_channel(application, identifier)?;
    channel.claims().map_err(|error| error.to_string())
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
    let finishable = should_finish_idle_change(
        application,
        project,
        change,
        Path::new(root),
        idle_nudge_exhausted(&thread),
    )?;
    if finishable && crate::threads::stop_idle_thread(application, key)? {
        let nick = if thread.started_by_dispatch && thread.issue.is_some() {
            dispatch_claims(application, project)?
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
        .finish_empty_dispatch_with_nick(project, thread.id, nick)
        .map_err(|error| error.to_string())?;
        crate::threads::refresh(application, key, false)?;
    }
    Ok(())
}

fn should_finish_idle_change(
    application: &AppHandle,
    project: u32,
    change: &str,
    root: &Path,
    exhausted: bool,
) -> Result<bool, String> {
    tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, project).await?;
        if exhausted {
            velocity_rosen::dispatch::is_unnamed_open_change(&server, change, None)
                .await
                .map_err(|error| error.to_string())
        } else {
            velocity_rosen::dispatch::ended_without_changes(&server, change, root)
                .await
                .map_err(|error| error.to_string())
        }
    })
}

fn report_no_change_issues(
    application: &AppHandle,
    project: &ProjectItem,
    reader: &mut Reader,
) -> Result<(), String> {
    for (&thread, pending) in &project.continuous_dispatch.no_change_reports {
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
        let result =
            IssueClient::for_repository(Path::new(&reader.directory), &pending.issue.repository)
                .and_then(|client| {
                    client.post_no_change_with_blocker_check(
                        pending.issue.number,
                        &pending.change,
                        &byline,
                        &pending.reason,
                        |issue| {
                            velocity_threads::continuous_dispatch::has_unsatisfied_blockers(
                                project,
                                &pending.issue.repository,
                                issue,
                            )
                        },
                    )
                });
        if let Err(error) = result {
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
