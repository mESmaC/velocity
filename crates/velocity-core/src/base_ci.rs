//! GitHub CI on a Rosen project's base (SPEC.md F5 "Rosen · Base CI", #507). Lands go
//! through after-land review and the integration build runs on one machine, so a job that
//! fails only on GitHub's runners is found here: the newest finished run of each workflow
//! decides, a failed job gets one fix-forward thread, and a later green run closes it.
//! Pure decisions; `velocity-github` reads GitHub and the app starts the threads.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The status Velocity publishes on a commit its integration build passed. It reports
/// Velocity's own build, which already has its fix-forward path, so it is not CI.
pub const INTEGRATION_CONTEXT: &str = "velocity/integration";

/// What the project shows for its base's CI.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum BaseCiState {
    /// No counted run has finished on the base and none is running.
    #[default]
    NotRun,
    /// Nothing failed, and a run is queued or in progress.
    Running,
    /// The newest finished run of every workflow passed.
    Green,
    /// The newest finished run of at least one workflow failed.
    Failed,
}

/// The CI state of a project's base on its GitHub mirror, kept with the project so it
/// shows at startup before the next read.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct BaseCi {
    #[serde(default)]
    pub state: BaseCiState,
    /// The base commit of the newest finished run.
    #[serde(default)]
    pub commit: Option<String>,
    /// The failing jobs as `<workflow> · <job>`.
    #[serde(default)]
    pub failing: Vec<String>,
    /// A newer run is queued or in progress, as after a fix lands.
    #[serde(default)]
    pub running: bool,
}

/// What one run says about the base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Passed,
    Failed,
    /// Queued or in progress.
    Running,
    /// Cancelled, skipped or superseded: it says nothing about its commit.
    Undecided,
}

/// A run's state from GitHub's `status` and `conclusion`. A newer push cancels the run
/// queued behind a running one, so a cancelled run is never a failure.
pub fn run_state(status: Option<&str>, conclusion: Option<&str>) -> RunState {
    if status != Some("completed") {
        return RunState::Running;
    }
    match conclusion {
        Some("success") => RunState::Passed,
        Some("failure" | "timed_out" | "startup_failure") => RunState::Failed,
        _ => RunState::Undecided,
    }
}

/// A commit status's state as a run state.
pub fn status_state(state: &str) -> RunState {
    match state {
        "success" => RunState::Passed,
        "failure" | "error" => RunState::Failed,
        "pending" => RunState::Running,
        _ => RunState::Undecided,
    }
}

/// Whether a run started by `event` is GitHub's own run of a base commit. A dispatched run
/// reports to whoever asked for it, and a pull request's run is not on the base.
pub fn counts_for_base(event: &str) -> bool {
    matches!(event, "push" | "schedule")
}

/// Whether a job with this conclusion failed. A job cancelled because a sibling failed did not.
pub fn job_failed(conclusion: Option<&str>) -> bool {
    matches!(conclusion, Some("failure" | "timed_out"))
}

/// One workflow run on the base branch, or one commit status on its tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseRun {
    /// The workflow's name, or the status's context.
    pub workflow: String,
    /// GitHub's run ID, which grows with each new run. A status has 0.
    pub run: u64,
    pub commit: String,
    pub url: String,
    pub state: RunState,
    /// What a status says about itself. A run's jobs and log say it instead.
    pub detail: String,
}

/// A job that failed, with the first step that failed in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailedJob {
    pub job: String,
    pub step: Option<String>,
    /// The runner labels, such as `macos-latest`.
    pub runner: Option<String>,
}

/// A workflow whose newest finished run failed, with the jobs that failed in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub workflow: String,
    pub run: u64,
    pub commit: String,
    pub url: String,
    pub detail: String,
    pub jobs: Vec<FailedJob>,
}

/// What the newest finished runs say, before any failed job is read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Workflows whose newest finished run passed.
    pub passed: Vec<String>,
    /// The newest finished run of each workflow that failed.
    pub failed: Vec<BaseRun>,
    /// A run newer than its workflow's newest finished one is queued or in progress.
    pub running: bool,
    /// The commit of the newest finished run of all.
    pub commit: Option<String>,
}

/// The newest finished run of each workflow decides it. An older failure under a newer
/// pass is fixed, and a run still going changes nothing until it finishes.
pub fn verdict(runs: &[BaseRun]) -> Verdict {
    let mut newest: BTreeMap<&str, &BaseRun> = BTreeMap::new();
    let decided = |run: &&BaseRun| matches!(run.state, RunState::Passed | RunState::Failed);
    for run in runs.iter().filter(decided) {
        let known = newest.entry(run.workflow.as_str()).or_insert(run);
        if run.run > known.run {
            *known = run;
        }
    }
    let running = runs.iter().any(|run| {
        run.state == RunState::Running
            && newest
                .get(run.workflow.as_str())
                .is_none_or(|finished| run.run >= finished.run)
    });
    let commit = newest
        .values()
        .max_by_key(|run| run.run)
        .map(|run| run.commit.clone());
    let (passed, failed): (Vec<_>, Vec<_>) = newest
        .into_values()
        .partition(|run| run.state == RunState::Passed);
    Verdict {
        passed: passed.into_iter().map(|run| run.workflow.clone()).collect(),
        failed: failed.into_iter().cloned().collect(),
        running,
        commit,
    }
}

/// A run that failed without a failed job, such as a workflow file GitHub could not start,
/// is one failure named for the run itself.
pub const WHOLE_RUN: &str = "workflow run";

/// The failure of `run` with the jobs GitHub listed as failed in it.
pub fn failure(run: &BaseRun, mut jobs: Vec<FailedJob>) -> Failure {
    if jobs.is_empty() {
        jobs.push(FailedJob {
            job: WHOLE_RUN.into(),
            ..FailedJob::default()
        });
    }
    Failure {
        workflow: run.workflow.clone(),
        run: run.run,
        commit: run.commit.clone(),
        url: run.url.clone(),
        detail: run.detail.clone(),
        jobs,
    }
}

/// The identity a failure is deduplicated by: one open repair per workflow and job.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepairKey {
    pub workflow: String,
    pub job: String,
}

/// What a look changes: the workflows whose repairs a green run closes, and the failures
/// that need a fix-forward thread, each reduced to the jobs no open repair covers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub closed: Vec<String>,
    pub start: Vec<Failure>,
}

/// A repeat of a job that already has an open repair starts nothing. Only a green run of
/// the same workflow closes a repair; a workflow with no finished run keeps its repairs.
pub fn plan(open: &[RepairKey], passed: &[String], failures: &[Failure]) -> Plan {
    let closed = passed
        .iter()
        .filter(|workflow| open.iter().any(|repair| repair.workflow == **workflow))
        .cloned()
        .collect();
    let start = failures
        .iter()
        .filter_map(|failure| {
            let jobs: Vec<FailedJob> = failure
                .jobs
                .iter()
                .filter(|job| {
                    !open
                        .iter()
                        .any(|repair| repair.workflow == failure.workflow && repair.job == job.job)
                })
                .cloned()
                .collect();
            (!jobs.is_empty()).then(|| Failure {
                jobs,
                ..failure.clone()
            })
        })
        .collect();
    Plan { closed, start }
}

/// What the project shows. A failure decides before a running check does: the base stays
/// failing until the workflow's next run finishes green.
pub fn view(verdict: &Verdict, failures: &[Failure]) -> BaseCi {
    let failing: Vec<String> = failures
        .iter()
        .flat_map(|failure| {
            failure
                .jobs
                .iter()
                .map(|job| job_label(&failure.workflow, &job.job))
        })
        .collect();
    let state = if !verdict.failed.is_empty() {
        BaseCiState::Failed
    } else if verdict.running {
        BaseCiState::Running
    } else if verdict.passed.is_empty() {
        BaseCiState::NotRun
    } else {
        BaseCiState::Green
    };
    BaseCi {
        state,
        commit: verdict.commit.clone(),
        failing,
        running: verdict.running,
    }
}

/// `<workflow> · <job>`, or the workflow alone for a status or a run with no failed job.
pub fn job_label(workflow: &str, job: &str) -> String {
    if job == workflow || job == WHOLE_RUN {
        workflow.to_owned()
    } else {
        format!("{workflow} · {job}")
    }
}

/// The lines `gh run view --log-failed` printed for `job`: each is the job, the step and
/// the log line, tab separated. The job and step columns, the line's timestamp and
/// terminal colours are dropped, so the excerpt spends its budget on the output.
pub fn job_log(log: &str, job: &str) -> String {
    let mut lines = Vec::new();
    for line in log.lines() {
        let mut columns = line.trim_start_matches('\u{feff}').splitn(3, '\t');
        let (Some(name), Some(_step), Some(text)) =
            (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        if name == job {
            lines.push(without_colours(without_timestamp(text)));
        }
    }
    lines.join("\n")
}

/// Drops the leading `2026-11-19T20:00:00.0000000Z ` GitHub puts on every log line.
fn without_timestamp(line: &str) -> &str {
    let line = line.trim_start_matches('\u{feff}');
    match line.split_once(' ') {
        Some((stamp, rest))
            if stamp.len() >= 20
                && stamp.ends_with('Z')
                && stamp.as_bytes().get(10) == Some(&b'T')
                && stamp.starts_with(|first: char| first.is_ascii_digit()) =>
        {
            rest
        }
        // A blank log line is the timestamp alone.
        None if line.len() >= 20
            && line.ends_with('Z')
            && line.as_bytes().get(10) == Some(&b'T') =>
        {
            ""
        }
        _ => line,
    }
}

/// Removes ANSI colour and cursor sequences (`ESC [ … letter`).
fn without_colours(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            plain.push(character);
            continue;
        }
        let mut sequence = characters.clone();
        if sequence.next() == Some('[') {
            for next in sequence.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            characters = sequence;
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(workflow: &str, run: u64, state: RunState) -> BaseRun {
        BaseRun {
            workflow: workflow.into(),
            run,
            commit: format!("{run:040}"),
            url: format!("https://github.com/owner/name/actions/runs/{run}"),
            state,
            detail: String::new(),
        }
    }

    fn failed_job(job: &str) -> FailedJob {
        FailedJob {
            job: job.into(),
            step: Some("Run tests".into()),
            runner: Some("macos-latest".into()),
        }
    }

    // Risk (#507): the three breaks of 2026-11-19 sat red on the base because nothing read
    // GitHub's runs. A failure must get exactly one repair per workflow and job, a
    // cancelled or still-running run must neither open nor close one, and only a green run
    // of the same workflow closes it.
    #[test]
    fn a_failure_gets_one_repair_until_a_green_run_of_its_workflow() {
        let runs = [
            run("Build", 10, RunState::Passed),
            run("Build", 11, RunState::Failed),
        ];
        let first = verdict(&runs);
        assert_eq!(first.failed, vec![runs[1].clone()]);
        let failures = [failure(&first.failed[0], vec![failed_job("test (macos)")])];
        assert_eq!(plan(&[], &first.passed, &failures).start, failures);
        assert_eq!(view(&first, &failures).state, BaseCiState::Failed);
        assert_eq!(view(&first, &failures).failing, ["Build · test (macos)"]);

        // The same job failing on the next base commit is a repeat; a new job is not.
        let open = [RepairKey {
            workflow: "Build".into(),
            job: "test (macos)".into(),
        }];
        let again = verdict(&[run("Build", 12, RunState::Failed)]);
        let failures = [failure(
            &again.failed[0],
            vec![failed_job("test (macos)"), failed_job("clippy")],
        )];
        let repeated = plan(&open, &again.passed, &failures);
        assert_eq!(repeated.start.len(), 1);
        assert_eq!(repeated.start[0].jobs, vec![failed_job("clippy")]);

        // The fix's run is going, a newer run was cancelled and another workflow is
        // green: the repair stays open and the base still shows failing.
        assert_eq!(
            run_state(Some("completed"), Some("cancelled")),
            RunState::Undecided
        );
        assert_eq!(run_state(Some("in_progress"), None), RunState::Running);
        let waiting = verdict(&[
            run("Build", 12, RunState::Failed),
            run("Build", 13, RunState::Undecided),
            run("Build", 14, RunState::Running),
            run("Docs", 15, RunState::Passed),
        ]);
        assert!(waiting.running);
        assert!(plan(&open, &waiting.passed, &[]).closed.is_empty());
        assert_eq!(view(&waiting, &[]).state, BaseCiState::Failed);

        let green = verdict(&[
            run("Build", 12, RunState::Failed),
            run("Build", 14, RunState::Passed),
        ]);
        assert_eq!(plan(&open, &green.passed, &[]).closed, ["Build"]);
        assert_eq!(view(&green, &[]).state, BaseCiState::Green);
        assert_eq!(view(&green, &[]).commit, Some(format!("{:040}", 14)));

        // The prompt's excerpt is the failed job's own output, without GitHub's columns.
        let log = "\u{feff}test (macos)\tRun tests\t2026-11-19T20:00:00.1234567Z \u{1b}[1m\u{1b}[31merror\u{1b}[0m: test failed\n\
                   test (macos)\tRun tests\t2026-11-19T20:00:01.1234567Z ##[error]Process completed with exit code 101.\n\
                   clippy\tRun clippy\t2026-11-19T20:00:02.1234567Z unused import";
        assert_eq!(
            job_log(log, "test (macos)"),
            "error: test failed\n##[error]Process completed with exit code 101."
        );
        assert_eq!(job_log(log, "clippy"), "unused import");
    }
}
