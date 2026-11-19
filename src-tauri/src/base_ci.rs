//! Watching GitHub CI on each Rosen project's mirrored base (SPEC.md F5 "Rosen · Base CI",
//! #507). Lands go through after-land review and the integration build runs on this
//! machine alone, so a job that fails only on GitHub's runners would sit red until somebody
//! read GitHub Actions. Every few minutes this reads each project's runs on its base
//! branch, starts one fix-forward thread per failing workflow run for the jobs that have
//! none, closes the repairs of a workflow whose newest run is green, and saves the state
//! the project shows. `velocity-core` decides and `velocity-github` talks to GitHub.

use std::collections::BTreeMap;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_core::base_ci::{self, BaseCi, BaseCiState, Failure, RepairKey};
use velocity_github::base_ci::{BaseCiWatch, FailedLog};
use velocity_store::VersionControl;
use velocity_threads::LaunchPlan;
use velocity_threads::ci_repairs::{CiFailure, pending_ci_repairs, prepare_ci_repair};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

/// The first look waits until startup work and resumed threads settle.
const FIRST_LOOK: Duration = Duration::from_secs(90);
/// A run takes far longer than this, and a look with nothing failing is two requests a
/// project, so a failure has its thread within minutes of the run finishing.
const LOOK_INTERVAL: Duration = Duration::from_secs(3 * 60);
const SOURCE: &str = "base ci";

/// A Rosen project whose base is mirrored to GitHub.
struct Subject {
    project: u32,
    name: String,
    /// The mirror's `owner/name`.
    repository: String,
    branch: String,
}

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    // `gh` blocks, so the looks have a thread of their own.
    std::thread::spawn(move || {
        // The failure last written to the log for each project, so one that repeats is
        // written once.
        let mut last_failures: BTreeMap<u32, String> = BTreeMap::new();
        std::thread::sleep(FIRST_LOOK);
        loop {
            for subject in subjects(&application) {
                let failure = look(&application, &subject).err();
                if let Some(failure) = &failure
                    && last_failures.get(&subject.project) != Some(failure)
                {
                    let message =
                        format!("{}: GitHub CI was not followed: {failure}", subject.name);
                    crate::diagnostics::record(SOURCE, &message);
                }
                match failure {
                    Some(failure) => last_failures.insert(subject.project, failure),
                    None => last_failures.remove(&subject.project),
                };
            }
            std::thread::sleep(LOOK_INTERVAL);
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
        .filter_map(|project| {
            let branch = project.default_branch.trim();
            Some(Subject {
                project: project.id,
                name: project.name.clone(),
                repository: project.github_remote.clone()?,
                branch: if branch.is_empty() { "master" } else { branch }.to_owned(),
            })
        })
        .collect()
}

/// One look at one project. A failed read changes nothing and is retried at the next look.
fn look(application: &AppHandle, subject: &Subject) -> Result<(), String> {
    let client = BaseCiWatch::new(&subject.repository).map_err(|error| error.to_string())?;
    let runs = client
        .runs(&subject.branch)
        .map_err(|error| error.to_string())?;
    let verdict = base_ci::verdict(&runs);
    let failures = verdict
        .failed
        .iter()
        .map(|run| {
            // A commit status has no run and no jobs: it is its own failure.
            let jobs = if run.run == 0 {
                Vec::new()
            } else {
                client
                    .failed_jobs(run.run)
                    .map_err(|error| error.to_string())?
            };
            Ok(base_ci::failure(run, jobs))
        })
        .collect::<Result<Vec<Failure>, String>>()?;
    let plan = base_ci::plan(
        &open_repairs(application, subject.project)?,
        &verdict.passed,
        &failures,
    );
    publish(application, subject, base_ci::view(&verdict, &failures))?;
    close(application, subject, &plan.closed)?;
    // A failure starting one repair must not hide another workflow's failure.
    let mut result = Ok(());
    for failure in &plan.start {
        result = result.and(start_repair(application, subject, &client, failure));
    }
    result.and(resume_pending(application, subject))
}

fn open_repairs(application: &AppHandle, project: u32) -> Result<Vec<RepairKey>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .project(project)
        .map(|item| {
            item.ci_repairs
                .iter()
                .map(|repair| RepairKey {
                    workflow: repair.workflow.clone(),
                    job: repair.job.clone(),
                })
                .collect()
        })
        .unwrap_or_default())
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
    let recovered = was_failing && view.state == BaseCiState::Green;
    let changed = store
        .save_base_ci(subject.project, Some(view))
        .map_err(|error| error.to_string())?;
    if changed {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        store.refresh_layout(&mut layout);
        if recovered {
            layout.show_toast(
                format!("{}: GitHub CI is green again on the base", subject.name),
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

/// Saves the repair and its thread, then starts the thread with the failed jobs' log.
fn start_repair(
    application: &AppHandle,
    subject: &Subject,
    client: &BaseCiWatch,
    failure: &Failure,
) -> Result<(), String> {
    // The thread can still open the run when its log could not be read.
    let log = if failure.run == 0 {
        FailedLog::default()
    } else {
        client.failed_log(failure.run).unwrap_or_default()
    };
    let report = CiFailure {
        repository: &subject.repository,
        branch: &subject.branch,
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
