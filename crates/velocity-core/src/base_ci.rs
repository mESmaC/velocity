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
    /// Normalized signature of the first concrete error in this job's failed log (#558).
    pub signature: Option<String>,
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

/// Lifecycle state of the thread owning an open CI repair (#558).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RepairThreadState {
    /// Thread is still working, in review, or waiting to launch.
    #[default]
    Active,
    /// Thread landed its change on the base.
    Landed,
    /// Thread finished or was abandoned without landing.
    Finished,
    /// Thread ended in error without landing.
    Error,
    /// Thread was removed from the project without landing.
    Deleted,
}

/// How a failing CI commit relates to a repair thread's landed commit (#558).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommitOrder {
    /// The failing commit is the landed commit itself or descends from it.
    AtOrAfterLand,
    /// The failing commit strictly predates the repair's land.
    BeforeLand,
    /// Commit ancestry could not be determined.
    #[default]
    Unknown,
}

/// An open repair evaluated by [`plan`] against the latest base CI runs (#558).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenRepair {
    pub workflow: String,
    pub job: String,
    /// Base commit the repair was opened for.
    pub commit: String,
    /// GitHub run ID the repair was opened for (0 for commit statuses).
    pub run: u64,
    pub thread: u32,
    /// Base commit landed by `thread`, once its change lands.
    pub landed_commit: Option<String>,
    /// Normalized error signature the repair was opened for, if extracted.
    pub signature: Option<String>,
    pub thread_state: RepairThreadState,
    /// How the current failing commit for `workflow` relates to `landed_commit`.
    pub commit_order: CommitOrder,
}

/// An open repair superseded by a failure at or after its land, or a new failure after
/// its thread ended (#558).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersededRepair {
    pub workflow: String,
    pub job: String,
    pub thread: u32,
    pub commit: String,
    pub landed_commit: Option<String>,
}

/// What a look changes: workflows whose repairs a green run closes, open repairs
/// superseded by a failure at or after their land, and failures needing a new thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub closed: Vec<String>,
    pub superseded: Vec<SupersededRepair>,
    pub start: Vec<Failure>,
}

/// Whether two extracted error signatures are both present and describe different errors.
pub fn signatures_differ(existing: Option<&str>, current: Option<&str>) -> bool {
    matches!((existing, current), (Some(left), Some(right)) if left != right)
}

/// Plans repair closures, reopens and new threads from the current base CI verdict (#558).
///
/// A green run of a workflow closes all of its repairs. A landed repair whose workflow is
/// still red at or after its land is superseded so a new fix-forward starts; while the
/// failing commit strictly predates the land, the repair stays open waiting for CI.
pub fn plan(open: &[OpenRepair], passed: &[String], failures: &[Failure]) -> Plan {
    let closed = passed
        .iter()
        .filter(|workflow| open.iter().any(|repair| repair.workflow == **workflow))
        .cloned()
        .collect();
    let mut superseded = Vec::new();
    for repair in open {
        let Some(failure) = failures
            .iter()
            .find(|failure| failure.workflow == repair.workflow)
        else {
            continue;
        };
        let matching_job = failure.jobs.iter().find(|job| job.job == repair.job);
        let stale = match matching_job {
            Some(job) => is_repair_superseded(repair, failure, job),
            None => {
                repair.thread_state == RepairThreadState::Landed
                    && effective_commit_order(repair, &failure.commit) == CommitOrder::AtOrAfterLand
            }
        };
        if stale {
            superseded.push(SupersededRepair {
                workflow: repair.workflow.clone(),
                job: repair.job.clone(),
                thread: repair.thread,
                commit: repair.commit.clone(),
                landed_commit: repair.landed_commit.clone(),
            });
        }
    }
    let start = failures
        .iter()
        .filter_map(|failure| {
            let jobs: Vec<FailedJob> = failure
                .jobs
                .iter()
                .filter(|job| {
                    !open
                        .iter()
                        .any(|repair| repair_covers_job(repair, failure, job))
                })
                .cloned()
                .collect();
            (!jobs.is_empty()).then(|| Failure {
                jobs,
                ..failure.clone()
            })
        })
        .collect();
    Plan {
        closed,
        superseded,
        start,
    }
}

fn effective_commit_order(repair: &OpenRepair, failing_commit: &str) -> CommitOrder {
    if repair
        .landed_commit
        .as_deref()
        .is_some_and(|landed| !landed.is_empty() && landed.eq_ignore_ascii_case(failing_commit))
    {
        return CommitOrder::AtOrAfterLand;
    }
    repair.commit_order
}

fn is_newer_run(repair: &OpenRepair, failure: &Failure) -> bool {
    failure.commit != repair.commit
        && (failure.run == 0 || repair.run == 0 || failure.run > repair.run)
}

fn is_repair_superseded(repair: &OpenRepair, failure: &Failure, job: &FailedJob) -> bool {
    let newer_run = is_newer_run(repair, failure);
    let different_error = signatures_differ(repair.signature.as_deref(), job.signature.as_deref());
    match repair.thread_state {
        RepairThreadState::Landed => match effective_commit_order(repair, &failure.commit) {
            CommitOrder::AtOrAfterLand => true,
            CommitOrder::BeforeLand => false,
            CommitOrder::Unknown => newer_run && different_error,
        },
        RepairThreadState::Finished => newer_run,
        RepairThreadState::Error | RepairThreadState::Deleted => {
            if repair.landed_commit.is_some()
                && effective_commit_order(repair, &failure.commit) == CommitOrder::AtOrAfterLand
            {
                true
            } else {
                newer_run && different_error
            }
        }
        RepairThreadState::Active => false,
    }
}

fn repair_covers_job(repair: &OpenRepair, failure: &Failure, job: &FailedJob) -> bool {
    if repair.workflow != failure.workflow || repair.job != job.job {
        return false;
    }
    if is_repair_superseded(repair, failure, job) {
        return false;
    }
    if repair.thread_state == RepairThreadState::Active
        && is_newer_run(repair, failure)
        && signatures_differ(repair.signature.as_deref(), job.signature.as_deref())
    {
        return false;
    }
    true
}

/// Formats the project health action line for a red GitHub CI base, naming the covered
/// commit and whether the latest failure is at or newer than the repair's land (#558).
pub fn format_ci_repair_action(
    repair: Option<&OpenRepair>,
    failing_commit: Option<&str>,
    spec_locked: bool,
) -> String {
    let Some(repair) = repair else {
        return if !spec_locked {
            "Cannot start fix-forward thread: project spec is not locked".into()
        } else {
            "Queued fix-forward thread on next GitHub CI check".into()
        };
    };
    let covered = short_sha(&repair.commit);
    let thread = repair.thread;
    match repair.thread_state {
        RepairThreadState::Active => match failing_commit {
            Some(latest) if !latest.eq_ignore_ascii_case(&repair.commit) => format!(
                "Fix-forward thread #{thread} active (covers {covered} · latest failure {})",
                short_sha(latest)
            ),
            _ => format!("Fix-forward thread #{thread} active (covers {covered})"),
        },
        RepairThreadState::Landed => format_landed_repair_action(repair, failing_commit, covered),
        RepairThreadState::Finished => {
            format!("Fix-forward thread #{thread} finished without landing (covered {covered})")
        }
        RepairThreadState::Error => {
            format!("Fix-forward thread #{thread} ended in error (covered {covered})")
        }
        RepairThreadState::Deleted => {
            format!("Fix-forward thread #{thread} deleted (covered {covered})")
        }
    }
}

fn format_landed_repair_action(
    repair: &OpenRepair,
    failing_commit: Option<&str>,
    covered: &str,
) -> String {
    let thread = repair.thread;
    let order = failing_commit.map_or(repair.commit_order, |commit| {
        effective_commit_order(repair, commit)
    });
    let Some(landed) = repair.landed_commit.as_deref() else {
        return format!("Fix-forward thread #{thread} landed (covered {covered}) · waiting for CI");
    };
    let land = short_sha(landed);
    match order {
        CommitOrder::AtOrAfterLand => match failing_commit {
            Some(latest) if landed.eq_ignore_ascii_case(latest) => format!(
                "Fix-forward thread #{thread} landed {land} (covered {covered}) · latest failure {land} is at its land (new fix-forward queued)"
            ),
            Some(latest) => format!(
                "Fix-forward thread #{thread} landed {land} (covered {covered}) · latest failure {} is newer than its land (new fix-forward queued)",
                short_sha(latest)
            ),
            None => format!(
                "Fix-forward thread #{thread} landed {land} (covered {covered}) · latest failure is at or newer than its land (new fix-forward queued)"
            ),
        },
        CommitOrder::BeforeLand => format!(
            "Fix-forward thread #{thread} landed {land} (covers {covered}) · waiting for CI on {land}"
        ),
        CommitOrder::Unknown => {
            format!(
                "Fix-forward thread #{thread} landed {land} (covers {covered}) · waiting for CI"
            )
        }
    }
}

fn short_sha(commit: &str) -> &str {
    let trimmed = commit.trim();
    if trimmed.is_empty() {
        "unknown"
    } else {
        trimmed.get(..12).unwrap_or(trimmed)
    }
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

/// Extracts a stable error signature from a job's stripped log: the first concrete
/// compiler/lint error (`error[...]` or `error: ...`) paired with its `--> <path>` file
/// (without line or column numbers), or a failing test / annotation line as fallback (#558).
pub fn error_signature(log: &str) -> Option<String> {
    let lines: Vec<&str> = log.lines().map(str::trim).collect();
    if let Some(signature) = first_compiler_error(&lines) {
        return Some(signature);
    }
    if let Some(signature) = first_typescript_error(&lines) {
        return Some(signature);
    }
    if let Some(signature) = first_test_failure(&lines) {
        return Some(signature);
    }
    first_annotation_error(&lines)
}

fn first_compiler_error(lines: &[&str]) -> Option<String> {
    for (index, line) in lines.iter().enumerate() {
        let is_coded = line.starts_with("error[");
        let is_plain = line.starts_with("error:") && !is_generic_error_summary(line);
        if !is_coded && !is_plain {
            continue;
        }
        let location = lines
            .get(index + 1..lines.len().min(index + 4))
            .and_then(|window| window.iter().copied().find_map(normalize_arrow_location));
        let headline = truncate_signature(line, 160);
        return Some(match location {
            Some(path) => format!("{headline} | --> {path}"),
            None => headline,
        });
    }
    None
}

fn is_generic_error_summary(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error: could not compile",
        "error: aborting due to",
        "error: test failed",
        "error: process didn't exit successfully",
        "error: build failed",
        "error: command exited",
        "error: failed to run custom build command",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
        || (lower.starts_with("error: ") && lower.contains("target failed"))
}

fn normalize_arrow_location(line: &str) -> Option<String> {
    let raw = line.strip_prefix("-->")?.trim();
    if raw.is_empty() {
        return None;
    }
    let without_line_col = strip_trailing_line_col(raw).replace('\\', "/");
    Some(truncate_signature(&without_line_col, 120))
}

fn strip_trailing_line_col(location: &str) -> &str {
    let mut trimmed = location;
    for _ in 0..2 {
        let Some((head, tail)) = trimmed.rsplit_once(':') else {
            break;
        };
        if !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit()) {
            trimmed = head;
        } else {
            break;
        }
    }
    trimmed
}

fn first_typescript_error(lines: &[&str]) -> Option<String> {
    for line in lines {
        let Some(position) = line.find("error TS") else {
            continue;
        };
        let message = truncate_signature(&line[position..], 160);
        let prefix = line[..position].trim().trim_end_matches(':').trim();
        let file = prefix
            .split_once('(')
            .map_or_else(|| strip_trailing_line_col(prefix), |(path, _)| path)
            .replace('\\', "/");
        return Some(if file.is_empty() {
            message
        } else {
            format!("{message} | --> {}", truncate_signature(&file, 120))
        });
    }
    None
}

fn first_test_failure(lines: &[&str]) -> Option<String> {
    for line in lines {
        if line.starts_with("test ") && line.ends_with("... FAILED") {
            return Some(truncate_signature(line, 180));
        }
        if let Some(rest) = line.strip_prefix("FAIL ") {
            let trimmed = rest.trim();
            if !trimmed.is_empty() {
                return Some(format!("FAIL {}", truncate_signature(trimmed, 160)));
            }
        }
    }
    None
}

fn first_annotation_error(lines: &[&str]) -> Option<String> {
    lines.iter().find_map(|line| {
        let rest = line.strip_prefix("##[error]")?.trim();
        if rest.is_empty() || rest.starts_with("Process completed with exit code") {
            None
        } else {
            Some(truncate_signature(rest, 180))
        }
    })
}

fn truncate_signature(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
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
            signature: None,
        }
    }

    fn failed_job_with_signature(job: &str, signature: &str) -> FailedJob {
        FailedJob {
            signature: Some(signature.into()),
            ..failed_job(job)
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
        let open = [OpenRepair {
            workflow: "Build".into(),
            job: "test (macos)".into(),
            commit: format!("{:040}", 11),
            run: 11,
            thread: 1,
            thread_state: RepairThreadState::Active,
            ..OpenRepair::default()
        }];
        let again = verdict(&[run("Build", 12, RunState::Failed)]);
        let failures = [failure(
            &again.failed[0],
            vec![failed_job("test (macos)"), failed_job("clippy")],
        )];
        let repeated = plan(&open, &again.passed, &failures);
        assert!(repeated.superseded.is_empty());
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

    // Risk (#558): when a fix-forward thread lands and leaves the base red on a different
    // error, keeping the old repair open stalls recovery indefinitely. A failure at or
    // after the repair's land must supersede the old repair and start a new fix-forward,
    // while an older failure before the land waits for CI.
    #[test]
    fn landed_repair_reopens_when_failure_is_at_or_after_its_land() {
        let landed_commit = format!("{:040}", 20);
        let original = OpenRepair {
            workflow: "Build and lint".into(),
            job: "clippy".into(),
            commit: format!("{:040}", 10),
            run: 10,
            thread: 7,
            landed_commit: Some(landed_commit),
            signature: Some(
                "error: redundant closure | --> crates/velocity-graph/src/lib.rs".into(),
            ),
            thread_state: RepairThreadState::Landed,
            commit_order: CommitOrder::BeforeLand,
        };

        // Failure predating the land: CI has not covered the land yet, so wait.
        let before = failure(
            &run("Build and lint", 11, RunState::Failed),
            vec![failed_job_with_signature(
                "clippy",
                "error: redundant closure | --> crates/velocity-graph/src/lib.rs",
            )],
        );
        let waiting = plan(std::slice::from_ref(&original), &[], &[before]);
        assert!(waiting.superseded.is_empty());
        assert!(waiting.start.is_empty());

        // Failure on the repair's own landed commit with a different error: reopen immediately.
        let at_land = failure(
            &run("Build and lint", 20, RunState::Failed),
            vec![failed_job_with_signature(
                "clippy",
                "error[E0063]: missing fields `change`, `payload` | --> crates/velocity-threads/src/agent_memory.rs",
            )],
        );
        let reopened = plan(
            &[OpenRepair {
                commit_order: CommitOrder::Unknown,
                ..original.clone()
            }],
            &[],
            std::slice::from_ref(&at_land),
        );
        assert_eq!(reopened.superseded.len(), 1);
        assert_eq!(reopened.superseded[0].thread, 7);
        assert_eq!(reopened.start, vec![at_land]);

        // Failure on a later commit descending from the land: also supersedes and restarts.
        let after_land = failure(
            &run("Build and lint", 25, RunState::Failed),
            vec![failed_job_with_signature(
                "clippy",
                "error: redundant closure | --> crates/velocity-graph/src/lib.rs",
            )],
        );
        let later = plan(
            &[OpenRepair {
                commit_order: CommitOrder::AtOrAfterLand,
                ..original
            }],
            &[],
            std::slice::from_ref(&after_land),
        );
        assert_eq!(later.superseded.len(), 1);
        assert_eq!(later.start, vec![after_land]);
    }

    // Risk (#558): error signatures distinguish independent breakages in the same job while
    // ignoring line-number shifts and generic Cargo summaries, and health notifications must
    // report the covered commit and whether the latest failure is newer than the land.
    #[test]
    fn error_signature_and_health_action_surface_distinct_and_stalled_failures() {
        let clippy_log = "\
error: redundant closure
  --> crates/velocity-graph/src/lib.rs:42:13
   |
42 |     .map(|x| foo(x))
error: could not compile `velocity-graph` (lib) due to 1 previous error";
        let shifted_clippy_log = "\
error: redundant closure
  --> crates/velocity-graph/src/lib.rs:99:13";
        let compile_log = "\
error: could not compile `velocity-threads`
error[E0063]: missing fields `change`, `payload` in initializer of `Entry`
   --> crates\\velocity-threads\\src\\agent_memory.rs:206:9";

        let clippy_sig = error_signature(clippy_log);
        let shifted_sig = error_signature(shifted_clippy_log);
        let compile_sig = error_signature(compile_log);
        assert_eq!(clippy_sig, shifted_sig);
        assert_eq!(
            compile_sig.as_deref(),
            Some(
                "error[E0063]: missing fields `change`, `payload` in initializer of `Entry` | --> crates/velocity-threads/src/agent_memory.rs"
            )
        );
        assert!(signatures_differ(
            clippy_sig.as_deref(),
            compile_sig.as_deref()
        ));

        // Active repair on error 1 does not suppress a distinct error 2 on a newer commit.
        let active = OpenRepair {
            workflow: "Build and lint".into(),
            job: "clippy".into(),
            commit: "40c32e0000000000000000000000000000000000".into(),
            run: 10,
            thread: 12,
            signature: clippy_sig,
            thread_state: RepairThreadState::Active,
            ..OpenRepair::default()
        };
        let second_error = failure(
            &run("Build and lint", 15, RunState::Failed),
            vec![FailedJob {
                signature: compile_sig,
                ..failed_job("clippy")
            }],
        );
        let distinct = plan(
            std::slice::from_ref(&active),
            &[],
            std::slice::from_ref(&second_error),
        );
        assert!(distinct.superseded.is_empty());
        assert_eq!(distinct.start, vec![second_error]);

        // Health text names the covered commit and flags a failure newer than the land.
        assert_eq!(
            format_ci_repair_action(Some(&active), Some(&active.commit), true),
            "Fix-forward thread #12 active (covers 40c32e000000)"
        );
        let landed = OpenRepair {
            landed_commit: Some("4101745000000000000000000000000000000000".into()),
            thread_state: RepairThreadState::Landed,
            commit_order: CommitOrder::AtOrAfterLand,
            ..active
        };
        let stalled_text = format_ci_repair_action(
            Some(&landed),
            Some("52a9f96000000000000000000000000000000000"),
            true,
        );
        assert!(stalled_text.contains("40c32e000000"));
        assert!(stalled_text.contains("410174500000"));
        assert!(stalled_text.contains("52a9f9600000"));
        assert!(stalled_text.contains("newer than its land"));
    }
}
