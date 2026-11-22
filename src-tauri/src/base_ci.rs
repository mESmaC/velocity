//! Watching GitHub CI on each Rosen project's mirrored base (SPEC.md F5 "Rosen · Base CI",
//! #507). Lands go through after-land review and the integration build runs on this
//! machine alone, so a job that fails only on GitHub's runners would sit red until somebody
//! read GitHub Actions. Every few minutes this reads each project's runs on its base
//! branch, starts one fix-forward thread per failing workflow run for the jobs that have
//! none, closes the repairs of a workflow whose newest run is green, and saves the state
//! the project shows. `velocity-core` decides and `velocity-github` talks to GitHub.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_core::base_ci::{
    self, BaseCi, BaseCiSource, BaseCiState, CommitOrder, Failure, OpenRepair, RepairThreadState,
    ResolvedJob, RosenBaseSnapshot, SupersededRepair,
};
use velocity_core::project_health::short_commit;
use velocity_core::thread_view_model::RosenChangeStage;
use velocity_github::base_ci::{BaseCiWatch, FailedLog};
use velocity_rosen::ProjectServer;
use velocity_rosen::landing::{self, LandCheckOutcome};
use velocity_rosen::post_land::{self, CommitAncestry};
use velocity_store::ci_repairs::CiRepair;
use velocity_store::{ThreadItem, ThreadState, VersionControl};
use velocity_threads::LaunchPlan;
use velocity_threads::ci_repairs::{
    CiFailure, RosenCiFailure, pending_ci_repairs, prepare_ci_repair, prepare_rosen_ci_repair,
};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

/// The first look waits until startup work and resumed threads settle.
const FIRST_LOOK: Duration = Duration::from_secs(90);
/// A run takes far longer than this, and a look with nothing failing is two requests a
/// project, so a failure has its thread within minutes of the run finishing.
const LOOK_INTERVAL: Duration = Duration::from_secs(3 * 60);
const SOURCE: &str = "base ci";

/// How base CI is observed for one Rosen project (#507, #591).
enum SubjectMode {
    /// GitHub-hosted project: watch GitHub Actions runs and commit statuses on the mirror.
    GitHub { repository: String, branch: String },
    /// Collapse or GitHub-free project: watch Rosen's per-platform post-land failures.
    Rosen,
}

/// A Rosen project watched for base CI failures.
struct Subject {
    project: u32,
    name: String,
    mode: SubjectMode,
}

struct BaseCiWake(std::sync::mpsc::Sender<()>);

/// Wakes the base CI poller immediately, e.g. when a project's GitHub repository changes (#574).
pub(crate) fn wake(application: &AppHandle) {
    if let Some(wake) = application.try_state::<BaseCiWake>() {
        let _ = wake.0.send(());
    }
}

pub fn initialize(application: &AppHandle) {
    let (sender, receiver) = std::sync::mpsc::channel();
    application.manage(BaseCiWake(sender));
    let application = application.clone();
    // `gh` blocks, so the looks have a thread of their own.
    std::thread::spawn(move || {
        // The failure last written to the log for each project, so one that repeats is
        // written once.
        let mut last_failures: BTreeMap<u32, String> = BTreeMap::new();
        // Cache failed-step logs by project and run ID so steady-state polls never re-fetch.
        let mut log_caches: BTreeMap<u32, BTreeMap<u64, FailedLog>> = BTreeMap::new();
        let _ = receiver.recv_timeout(FIRST_LOOK);
        loop {
            while receiver.try_recv().is_ok() {}
            for subject in subjects(&application) {
                let run_logs = log_caches.entry(subject.project).or_default();
                let failure = look(&application, &subject, run_logs).err();
                if let Some(failure) = &failure
                    && last_failures.get(&subject.project) != Some(failure)
                {
                    let label = match subject.mode {
                        SubjectMode::GitHub { .. } => "GitHub CI",
                        SubjectMode::Rosen => "Rosen post-land CI",
                    };
                    let message = format!("{}: {label} was not followed: {failure}", subject.name);
                    crate::diagnostics::record(SOURCE, &message);
                }
                match failure {
                    Some(failure) => last_failures.insert(subject.project, failure),
                    None => last_failures.remove(&subject.project),
                };
            }
            let _ = receiver.recv_timeout(LOOK_INTERVAL);
        }
    });
}

fn subjects(application: &AppHandle) -> Vec<Subject> {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return Vec::new();
    };
    let Ok(store) = store.lock() else {
        return Vec::new();
    };
    store
        .state()
        .projects
        .iter()
        .filter(|project| project.version_control == VersionControl::Rosen)
        .map(|project| {
            let mode = match project.effective_base_ci_source() {
                BaseCiSource::Rosen => SubjectMode::Rosen,
                BaseCiSource::GitHub => match project.github_remote.clone() {
                    Some(repository) => {
                        let branch = project.default_branch.trim();
                        SubjectMode::GitHub {
                            repository,
                            branch: if branch.is_empty() { "master" } else { branch }.to_owned(),
                        }
                    }
                    None => SubjectMode::Rosen,
                },
            };
            Subject {
                project: project.id,
                name: project.name.clone(),
                mode,
            }
        })
        .collect()
}

/// One look at one project. A failed read changes nothing and is retried at the next look.
fn look(
    application: &AppHandle,
    subject: &Subject,
    run_logs: &mut BTreeMap<u64, FailedLog>,
) -> Result<(), String> {
    match &subject.mode {
        SubjectMode::GitHub { repository, branch } => {
            // Never follow GitHub Actions if `[mirror] remote` in `rosen.toml` is Collapse (#613),
            // even before `rosen_mirror::refresh` updates `collapse_repository` in persisted state.
            if mirror_remote_is_collapse(application, subject.project) {
                return look_rosen(application, subject);
            }
            look_github(application, subject, repository, branch, run_logs)
        }
        SubjectMode::Rosen => look_rosen(application, subject),
    }
}

fn mirror_remote_is_collapse(application: &AppHandle, project: u32) -> bool {
    if crate::rosen::paused(application) {
        return false;
    }
    let Ok(server) = crate::rosen::observation_server(application, project) else {
        return false;
    };
    tauri::async_runtime::block_on(velocity_rosen::mirror_remote(&server))
        .ok()
        .flatten()
        .is_some_and(|remote| velocity_core::collapse::repository_from_remote(&remote).is_some())
}

fn look_github(
    application: &AppHandle,
    subject: &Subject,
    repository: &str,
    branch: &str,
    run_logs: &mut BTreeMap<u64, FailedLog>,
) -> Result<(), String> {
    let client = BaseCiWatch::new(repository).map_err(|error| error.to_string())?;
    let snapshot = client.runs(branch).map_err(|error| error.to_string())?;
    let mut verdict = base_ci::evaluate_jobs(&snapshot.runs, &snapshot.run_jobs);
    let open = open_repairs(application, subject.project, &verdict.active_failures)?;
    populate_signatures(
        &client,
        &open,
        &mut verdict.active_failures,
        &verdict.waiting_failures,
        run_logs,
    );
    let plan = base_ci::plan_for_verdict(&open, &verdict);
    publish(application, subject, base_ci::view_for_verdict(&verdict))?;
    close(application, subject, &plan.closed)?;
    close_resolved(application, subject, &plan.resolved)?;
    close_superseded(application, subject, &plan.superseded)?;
    // A failure starting one repair must not hide another workflow's failure.
    let mut result = Ok(());
    for failure in &plan.start {
        result = result.and(start_repair(
            application,
            subject,
            repository,
            branch,
            &client,
            failure,
            run_logs.get(&failure.run),
        ));
    }
    result.and(resume_pending(application, subject))
}

/// Observes Rosen per-platform post-land failures and checkpoints on Collapse or GitHub-free
/// projects without calling GitHub Actions (#591).
fn look_rosen(application: &AppHandle, subject: &Subject) -> Result<(), String> {
    if crate::rosen::paused(application) {
        return Ok(());
    }
    let server = crate::rosen::observation_server(application, subject.project)?;
    let snapshot = tauri::async_runtime::block_on(post_land::base_ci_snapshot(&server))
        .map_err(|error| error.to_string())?;
    let (verdict, view) = base_ci::evaluate_rosen_post_land(&snapshot);
    let open = open_repairs(application, subject.project, &verdict.active_failures)?;
    let plan = base_ci::plan_for_verdict(&open, &verdict);
    publish(application, subject, view)?;
    close(application, subject, &plan.closed)?;
    close_resolved(application, subject, &plan.resolved)?;
    close_superseded(application, subject, &plan.superseded)?;
    let mut result = Ok(());
    for failure in &plan.start {
        result = result.and(start_rosen_repair(application, subject, &snapshot, failure));
    }
    result.and(resume_pending(application, subject))
}

fn open_repairs(
    application: &AppHandle,
    project: u32,
    failures: &[Failure],
) -> Result<Vec<OpenRepair>, String> {
    let server = application
        .path()
        .app_data_dir()
        .ok()
        .map(|app_data| ProjectServer::new(&app_data, project));
    let snapshots = repair_snapshots(application, project)?;
    let mut open = Vec::with_capacity(snapshots.len());
    let mut newly_landed = Vec::new();
    for (repair, thread, live) in snapshots {
        let landed_commit = tauri::async_runtime::block_on(resolve_repair_landed_commit(
            application,
            server.as_ref(),
            &repair,
            thread.as_ref(),
        ));
        if repair.landed_commit.is_none()
            && let Some(commit) = &landed_commit
        {
            newly_landed.push((repair.thread, commit.clone()));
        }
        let thread_state = repair_thread_state(thread.as_ref(), landed_commit.as_deref(), live);
        let matching_failure = failures
            .iter()
            .find(|failure| {
                failure.workflow == repair.workflow
                    && failure.jobs.iter().any(|job| job.job == repair.job)
            })
            .or_else(|| {
                failures
                    .iter()
                    .filter(|failure| failure.workflow == repair.workflow)
                    .max_by_key(|failure| failure.run)
            });
        let commit_order =
            match (landed_commit.as_deref(), matching_failure) {
                (Some(landed), Some(failure)) => tauri::async_runtime::block_on(
                    resolve_commit_order(application, server.as_ref(), landed, &failure.commit),
                ),
                _ => CommitOrder::Unknown,
            };
        open.push(OpenRepair {
            workflow: repair.workflow,
            job: repair.job,
            commit: repair.commit,
            run: repair.run,
            thread: repair.thread,
            landed_commit,
            signature: repair.signature,
            thread_state,
            commit_order,
        });
    }
    record_newly_landed(application, project, &newly_landed)?;
    Ok(open)
}

fn repair_snapshots(
    application: &AppHandle,
    project: u32,
) -> Result<Vec<(CiRepair, Option<ThreadItem>, bool)>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(item) = store.project(project) else {
        return Ok(Vec::new());
    };
    let threads = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?;
    Ok(item
        .ci_repairs
        .iter()
        .map(|repair| {
            let thread = item
                .threads
                .iter()
                .find(|candidate| candidate.id == repair.thread)
                .cloned();
            let live = threads.is_live((project, repair.thread));
            (repair.clone(), thread, live)
        })
        .collect())
}

fn record_newly_landed(
    application: &AppHandle,
    project: u32,
    newly_landed: &[(u32, String)],
) -> Result<(), String> {
    if newly_landed.is_empty() {
        return Ok(());
    }
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    for (thread, commit) in newly_landed {
        let _ = store.record_ci_repair_landed(project, *thread, commit);
    }
    Ok(())
}

/// Classifies the thread owning a CI repair for planning and health display (#558).
pub(crate) fn repair_thread_state(
    thread: Option<&ThreadItem>,
    landed_commit: Option<&str>,
    live: bool,
) -> RepairThreadState {
    if landed_commit.is_some() {
        return RepairThreadState::Landed;
    }
    let Some(thread) = thread else {
        return RepairThreadState::Deleted;
    };
    if thread.state == ThreadState::Merged {
        return RepairThreadState::Landed;
    }
    if thread.pending_rosen_launch.is_some()
        || live
        || matches!(
            thread.state,
            ThreadState::Draft | ThreadState::Spec | ThreadState::Working | ThreadState::Review
        )
    {
        return RepairThreadState::Active;
    }
    match thread.state {
        ThreadState::Error => RepairThreadState::Error,
        _ => RepairThreadState::Finished,
    }
}

/// Resolves the landed base commit for `repair` from its saved field, the thread's cached
/// Rosen change view, or Rosen's read-only `land_check` when the server is unpaused (#558).
pub(crate) async fn resolve_repair_landed_commit(
    application: &AppHandle,
    server: Option<&ProjectServer>,
    repair: &CiRepair,
    thread: Option<&ThreadItem>,
) -> Option<String> {
    if let Some(commit) = repair
        .landed_commit
        .as_deref()
        .filter(|commit| !commit.trim().is_empty())
    {
        return Some(commit.to_owned());
    }
    let thread = thread?;
    if let Some(view) = &thread.rosen_change_view
        && matches!(
            view.stage,
            RosenChangeStage::Landed | RosenChangeStage::Green
        )
        && let Some(land) = view.lands.last()
        && !land.commit.trim().is_empty()
    {
        return Some(land.commit.clone());
    }
    let server = server?;
    if crate::rosen::paused(application) {
        return None;
    }
    let change = thread.rosen_change.as_deref()?;
    match landing::land_check(server, change).await {
        Ok(LandCheckOutcome::AlreadyLanded { commit }) if !commit.trim().is_empty() => Some(commit),
        _ => None,
    }
}

/// Compares `failing_commit` against `landed_commit` in Rosen's base history (#558).
pub(crate) async fn resolve_commit_order(
    application: &AppHandle,
    server: Option<&ProjectServer>,
    landed_commit: &str,
    failing_commit: &str,
) -> CommitOrder {
    if landed_commit.eq_ignore_ascii_case(failing_commit) {
        return CommitOrder::AtOrAfterLand;
    }
    let Some(server) = server else {
        return CommitOrder::Unknown;
    };
    if crate::rosen::paused(application) {
        return CommitOrder::Unknown;
    }
    match post_land::compare_commits(server, landed_commit, failing_commit).await {
        Ok(CommitAncestry::Same | CommitAncestry::Descendant) => CommitOrder::AtOrAfterLand,
        Ok(CommitAncestry::Ancestor) => CommitOrder::BeforeLand,
        Ok(CommitAncestry::Diverged | CommitAncestry::Unknown) | Err(_) => CommitOrder::Unknown,
    }
}

fn populate_signatures(
    client: &BaseCiWatch,
    open: &[OpenRepair],
    failures: &mut [Failure],
    waiting: &[Failure],
    run_logs: &mut BTreeMap<u64, FailedLog>,
) {
    let active_runs: HashSet<u64> = failures
        .iter()
        .chain(waiting)
        .filter(|failure| failure.run != 0)
        .map(|failure| failure.run)
        .collect();
    run_logs.retain(|run, _| active_runs.contains(run));
    for failure in failures {
        if !needs_log(open, failure) {
            continue;
        }
        let log = match run_logs.entry(failure.run) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut().text.as_str(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                match client.failure_log(failure) {
                    Ok(fetched) => entry.insert(fetched).text.as_str(),
                    Err(_) => "",
                }
            }
        };
        for job in &mut failure.jobs {
            let job_output = base_ci::job_log(log, &job.job);
            if job.timed_out {
                job.hung_test = base_ci::hung_test(&job_output);
            }
            job.signature = job
                .hung_test
                .as_ref()
                .map(|test| format!("timed out: {test}"))
                .or_else(|| base_ci::error_signature(&job_output));
        }
    }
}

fn needs_log(open: &[OpenRepair], failure: &Failure) -> bool {
    if failure.run == 0 {
        return false;
    }
    failure.jobs.iter().any(|job| {
        let matching: Vec<&OpenRepair> = open
            .iter()
            .filter(|repair| repair.workflow == failure.workflow && repair.job == job.job)
            .collect();
        if matching.is_empty() {
            return true;
        }
        !matching.iter().any(|repair| {
            (repair.run == failure.run && repair.thread_state != RepairThreadState::Landed)
                || (repair.thread_state == RepairThreadState::Landed
                    && repair.commit_order == CommitOrder::BeforeLand)
        })
    })
}

/// Saves the state and republishes the window when it changed. Turning green after a
/// failure says so in a toast, since nothing else marks the end of a repair.
fn publish(application: &AppHandle, subject: &Subject, view: BaseCi) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let was_failing = store
        .project(subject.project)
        .and_then(|item| item.base_ci.as_ref())
        .is_some_and(|ci| ci.state == BaseCiState::Failed);
    let source = view.source;
    let recovered = was_failing && view.state == BaseCiState::Green;
    let changed = store
        .save_base_ci(subject.project, Some(view))
        .map_err(|error| error.to_string())?;
    if changed {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        store.refresh_layout(&mut layout);
        if recovered {
            let label = match source {
                BaseCiSource::GitHub => "GitHub CI is",
                BaseCiSource::Rosen => "Rosen post-land checks are",
            };
            layout.show_toast(
                format!("{}: {label} green again on the base", subject.name),
                None,
            );
        }
        crate::layout::publish(application, &layout.snapshot(), None)?;
    }
    Ok(())
}

fn close(application: &AppHandle, subject: &Subject, workflows: &[String]) -> Result<(), String> {
    if workflows.is_empty() {
        return Ok(());
    }
    let closed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .close_ci_repairs(subject.project, workflows)
    .map_err(|error| error.to_string())?;
    if !closed.is_empty() {
        let message = format!(
            "{}: {} passed on the base · closed {} CI repair(s)",
            subject.name,
            workflows.join(", "),
            closed.len()
        );
        crate::diagnostics::record(SOURCE, &message);
    }
    Ok(())
}

fn close_resolved(
    application: &AppHandle,
    subject: &Subject,
    resolved: &[ResolvedJob],
) -> Result<(), String> {
    if resolved.is_empty() {
        return Ok(());
    }
    let closed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .close_resolved_ci_repairs(subject.project, resolved)
    .map_err(|error| error.to_string())?;
    for repair in &closed {
        let job = base_ci::job_label(&repair.workflow, &repair.job);
        let passer = resolved
            .iter()
            .find(|item| item.workflow == repair.workflow && item.job == repair.job);
        let detail = match passer {
            Some(item) if !item.commit.trim().is_empty() => format!(
                "passed on {} (superseded thread #{} covering {})",
                short_commit(&item.commit),
                repair.thread,
                short_commit(&repair.commit)
            ),
            _ => format!(
                "passed on a newer run (superseded thread #{} covering {})",
                repair.thread,
                short_commit(&repair.commit)
            ),
        };
        let message = format!("{}: closed CI repair for {job} ({detail})", subject.name);
        crate::diagnostics::record(SOURCE, &message);
    }
    Ok(())
}

fn close_superseded(
    application: &AppHandle,
    subject: &Subject,
    superseded: &[SupersededRepair],
) -> Result<(), String> {
    if superseded.is_empty() {
        return Ok(());
    }
    let closed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .close_superseded_ci_repairs(subject.project, superseded)
    .map_err(|error| error.to_string())?;
    for repair in &closed {
        let job = base_ci::job_label(&repair.workflow, &repair.job);
        let detail = match repair.landed_commit.as_deref() {
            Some(landed) => format!(
                "thread #{} landed {} (covered {})",
                repair.thread,
                short_commit(landed),
                short_commit(&repair.commit)
            ),
            None => format!(
                "thread #{} finished without landing (covered {})",
                repair.thread,
                short_commit(&repair.commit)
            ),
        };
        let message = format!(
            "{}: reopened CI repair for {job} ({detail} · base still red)",
            subject.name
        );
        crate::diagnostics::record(SOURCE, &message);
    }
    Ok(())
}

/// Saves the repair and its thread, then starts the thread with the failed jobs' log.
fn start_repair(
    application: &AppHandle,
    subject: &Subject,
    repository: &str,
    branch: &str,
    client: &BaseCiWatch,
    failure: &Failure,
    cached_log: Option<&FailedLog>,
) -> Result<(), String> {
    // The thread can still open the run when its log could not be read.
    let owned_log;
    let log = match cached_log {
        Some(cached) => cached,
        None => {
            owned_log = if failure.run == 0 {
                FailedLog::default()
            } else {
                client.failure_log(failure).unwrap_or_default()
            };
            &owned_log
        }
    };
    let report = CiFailure {
        repository,
        branch,
        failure,
        log: &log.text,
        saved_log: log.saved.as_deref(),
    };
    let planned = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        prepare_ci_repair(&mut store, subject.project, &report)?
    };
    let Some((plan, thread)) = planned else {
        return Ok(());
    };
    let jobs: Vec<String> = failure
        .jobs
        .iter()
        .map(|job| base_ci::job_label(&failure.workflow, &job.job))
        .collect();
    let message = format!(
        "{}: GitHub CI failed on the base ({}) · started a fix-forward thread",
        subject.name,
        jobs.join(", ")
    );
    crate::diagnostics::record(SOURCE, &message);
    launch(application, plan, thread)?;
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

/// Saves and launches a fix-forward thread for a Rosen per-platform post-land failure (#591).
fn start_rosen_repair(
    application: &AppHandle,
    subject: &Subject,
    snapshot: &RosenBaseSnapshot,
    failure: &Failure,
) -> Result<(), String> {
    let source = snapshot
        .failures
        .iter()
        .find(|entry| entry.session == failure.run);
    let change = source.map_or("", |entry| entry.change.as_str());
    let platform = source
        .and_then(|entry| entry.platform_display.as_deref())
        .or_else(|| failure.jobs.first().and_then(|job| job.runner.as_deref()))
        .unwrap_or("base");
    let check_kind = source.map_or("test", |entry| entry.kind.as_str());
    let output = source.map_or("", |entry| entry.output.as_str());
    let saved_log = save_rosen_failure_log(failure.run, output);
    let report = RosenCiFailure {
        project: &subject.name,
        failure,
        change,
        platform,
        check_kind,
        output,
        saved_log: saved_log.as_deref(),
    };
    let planned = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        prepare_rosen_ci_repair(&mut store, subject.project, &report)?
    };
    let Some((plan, thread)) = planned else {
        return Ok(());
    };
    let message = format!(
        "{}: Rosen post-land {check_kind} failed on {platform} · started a fix-forward thread",
        subject.name,
    );
    crate::diagnostics::record(SOURCE, &message);
    launch(application, plan, thread)?;
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

fn save_rosen_failure_log(session: u64, output: &str) -> Option<PathBuf> {
    if output.trim().is_empty() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("velocity-base-ci-rosen-{session}"));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("failed-steps.log");
    std::fs::write(&path, output).ok()?;
    Some(path)
}

/// Starts the repairs that were saved and never started: Velocity stopped before the
/// launch, or the project's server was down.
fn resume_pending(application: &AppHandle, subject: &Subject) -> Result<(), String> {
    let pending = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        pending_ci_repairs(&store, subject.project)
    };
    let mut result = Ok(());
    for (plan, thread) in pending {
        let live = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?
        .is_live((subject.project, thread));
        if !live {
            result = result.and(launch(application, plan, thread));
        }
    }
    result
}

fn launch(application: &AppHandle, plan: LaunchPlan, thread: u32) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(plan.project, thread, |thread| {
        thread.state = velocity_store::ThreadState::Working;
        thread.activity = velocity_store::AgentActivity::Executing;
    })
    .map_err(|error| error.to_string())?;
    // Persist the thread identity and opening prompt before any process starts.
    crate::state::save_now(application)?;
    crate::threads::start_planned_with_selection(application, plan, thread, false)?;
    Ok(())
}
