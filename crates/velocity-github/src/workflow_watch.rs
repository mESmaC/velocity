//! Runs a GitHub workflow for an agent from start to result (#508): dispatch it, wait for
//! the run, then collect what the agent is told. Everything here blocks on `gh`, so the
//! caller gives it a thread of its own and a way to say the session has stopped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::workflow_report::{Detail, JobReport, RunReport};
use crate::workflow_runs::{WorkflowClient, WorkflowJob, WorkflowRun};
use crate::{GitHubCommandError, workflow_artifacts};

/// GitHub ends a job after six hours, so a run still going after this is one waiting on
/// something no agent can give it, such as an approval.
const RUN_LIMIT: Duration = Duration::from_secs(6 * 60 * 60);
/// Most runs an agent asks for take minutes. A long one is asked about less often.
const EARLY_POLL: Duration = Duration::from_secs(15);
const LATE_POLL: Duration = Duration::from_secs(45);
const EARLY: Duration = Duration::from_secs(15 * 60);
/// Failed reads in a row before the wait is given up: two minutes of GitHub not answering.
const MAXIMUM_FAILED_READS: u32 = 8;
/// Jobs whose annotations are read. A matrix can have hundreds; each is a request.
const MAXIMUM_ANNOTATED_JOBS: usize = 30;

/// A workflow to run: a file under `.github/workflows` that the owner allows.
#[derive(Clone, Debug)]
pub struct WorkflowDispatch {
    pub workflow: String,
    /// The branch or tag to run it at. `None` is the repository's default branch.
    pub reference: Option<String>,
    pub inputs: BTreeMap<String, String>,
}

/// What became of a requested run, worded for the agent's next prompt.
#[derive(Clone, Debug)]
pub struct WorkflowOutcome {
    /// Whether the run concluded `success`.
    pub succeeded: bool,
    pub summary: String,
}

/// Dispatches `dispatch` in the repository `mirror` names and waits for the run. `stopped`
/// turns true when nothing waits for the result any more; the run itself is left alone.
#[track_caller]
pub fn run_workflow(
    mirror: &str,
    dispatch: &WorkflowDispatch,
    stopped: &dyn Fn() -> bool,
) -> WorkflowOutcome {
    let failed = |reason: String| WorkflowOutcome {
        succeeded: false,
        summary: format!("Workflow {}: {reason}", dispatch.workflow),
    };
    let client = match WorkflowClient::for_mirror(mirror) {
        Ok(client) => client,
        Err(error) => return failed(error.to_string()),
    };
    let started = start(&client, dispatch, stopped);
    let (reference, run) = match started {
        Ok(started) => started,
        Err(error) => return failed(format!("GitHub did not start it. {error}")),
    };
    match wait(&client, run, stopped) {
        Ok(run) => report(&client, dispatch, &reference, &run),
        Err(reason) => failed(reason),
    }
}

fn start(
    client: &WorkflowClient,
    dispatch: &WorkflowDispatch,
    stopped: &dyn Fn() -> bool,
) -> Result<(String, WorkflowRun), GitHubCommandError> {
    let reference = match &dispatch.reference {
        Some(reference) => reference.clone(),
        None => client.default_branch()?,
    };
    let run = client.dispatch(&dispatch.workflow, &reference, &dispatch.inputs, stopped)?;
    Ok((reference, run))
}

/// The run once it has completed, or why the wait ended before that.
fn wait(
    client: &WorkflowClient,
    mut run: WorkflowRun,
    stopped: &dyn Fn() -> bool,
) -> Result<WorkflowRun, String> {
    let started = Instant::now();
    let mut failed_reads = 0;
    while !run.completed() {
        let url = run.html_url.clone();
        if stopped() {
            return Err(format!(
                "the session stopped before the run finished. The run continues at {url}"
            ));
        }
        if started.elapsed() > RUN_LIMIT {
            return Err(format!("still running after six hours. See {url}"));
        }
        pause(started.elapsed(), stopped);
        match client.run(run.id) {
            Ok(latest) => {
                run = latest;
                failed_reads = 0;
            }
            Err(error) if failed_reads >= MAXIMUM_FAILED_READS => {
                return Err(format!(
                    "could not read the run's state ({error}). The run continues at {url}"
                ));
            }
            Err(_) => failed_reads += 1,
        }
    }
    Ok(run)
}

/// Sleeps until the next read is due, waking early when the session stops.
fn pause(waited: Duration, stopped: &dyn Fn() -> bool) {
    let interval = if waited < EARLY {
        EARLY_POLL
    } else {
        LATE_POLL
    };
    let until = Instant::now() + interval;
    while Instant::now() < until && !stopped() {
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn report(
    client: &WorkflowClient,
    dispatch: &WorkflowDispatch,
    reference: &str,
    run: &WorkflowRun,
) -> WorkflowOutcome {
    let folder = std::env::temp_dir().join(format!("velocity-workflow-{}", run.id));
    let mut notes = Vec::new();
    if let Err(error) = std::fs::create_dir_all(&folder) {
        notes.push(format!("Could not create {}: {error}", folder.display()));
    }
    let jobs = noted(client.jobs(run.id), "the jobs", &mut notes);
    // A cancelled or skipped run has no failed step, and `gh` answers that with an error.
    let step_failed = jobs
        .iter()
        .any(|job| matches!(job.conclusion.as_deref(), Some("failure" | "timed_out")));
    let failed_log = step_failed
        .then(|| save_failed_log(client, run.id, &folder, &mut notes))
        .flatten();
    let jobs = job_reports(client, jobs);
    let listed = noted(client.artifacts(run.id), "the artifacts", &mut notes);
    let artifacts = workflow_artifacts::download(client, run.id, listed, &folder.join("artifacts"));
    let report = RunReport {
        workflow: &dispatch.workflow,
        reference,
        repository: client.repository(),
        run,
        jobs: &jobs,
        artifacts: &artifacts,
        failed_log: failed_log
            .as_ref()
            .map(|(log, saved)| (log.as_str(), saved.as_path())),
        folder: &folder,
        notes: &notes,
    };
    // The full report is a convenience beside the artifacts; the prompt carries the result.
    let _ = std::fs::write(folder.join("report.md"), report.render(&Detail::FULL));
    WorkflowOutcome {
        succeeded: run.conclusion.as_deref() == Some("success"),
        summary: report.render(&Detail::PROMPT),
    }
}

/// `read`'s value, or nothing with a note saying which part of the run could not be read.
fn noted<T: Default>(
    read: Result<T, GitHubCommandError>,
    part: &str,
    notes: &mut Vec<String>,
) -> T {
    read.unwrap_or_else(|error| {
        notes.push(format!("Could not read {part}: {error}"));
        T::default()
    })
}

fn job_reports(client: &WorkflowClient, jobs: Vec<WorkflowJob>) -> Vec<JobReport> {
    let mut annotated = 0;
    jobs.into_iter()
        .map(|job| {
            // A skipped job ran nothing, so it has nothing to say.
            let read =
                job.conclusion.as_deref() != Some("skipped") && annotated < MAXIMUM_ANNOTATED_JOBS;
            let annotations = if read {
                annotated += 1;
                client.annotations(job.id).unwrap_or_default()
            } else {
                Vec::new()
            };
            JobReport { job, annotations }
        })
        .collect()
}

/// The failed steps' output, saved whole beside the report.
fn save_failed_log(
    client: &WorkflowClient,
    run: u64,
    folder: &Path,
    notes: &mut Vec<String>,
) -> Option<(String, PathBuf)> {
    let log = noted(client.failed_log(run), "the failed steps' log", notes);
    if log.trim().is_empty() {
        return None;
    }
    let saved = folder.join("failed-steps.log");
    if let Err(error) = std::fs::write(&saved, &log) {
        notes.push(format!("Could not save {}: {error}", saved.display()));
    }
    Some((log, saved))
}
