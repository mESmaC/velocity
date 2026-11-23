//! GitHub CI on a Rosen project's base (SPEC.md F5 "Rosen · Base CI", #507). Lands go
//! through after-land review and the integration build runs on one machine, so a job that
//! fails only on GitHub's runners is found here: each job is decided by the newest run in
//! which that job finished (whether or not the rest of the run finished, #587), a failed
//! job gets one fix-forward thread, and a later passing run closes it.
//! Pure decisions; `velocity-github` reads GitHub and the app starts the threads.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The status Velocity publishes on a commit its integration build passed. It reports
/// Velocity's own build, which already has its fix-forward path, so it is not CI.
pub const INTEGRATION_CONTEXT: &str = "velocity/integration";

/// The GitHub App slug used by GitHub Actions check runs (#652).
pub const GITHUB_ACTIONS_APP_SLUG: &str = "github-actions";

/// The GitHub App ID used by GitHub Actions on github.com (#652).
pub const GITHUB_ACTIONS_APP_ID: u64 = 15_368;

/// Substring GitHub writes in a check-run annotation when a job exceeds `timeout-minutes` (#584).
pub const TIMEOUT_ANNOTATION_NEEDLE: &str = "has exceeded the maximum execution time";

/// Marker Rust's test harness writes for a test that has been running for over 60 seconds (#584).
pub const HUNG_TEST_MARKER: &str = "has been running for over 60 seconds";

/// GitHub Actions' default job `timeout-minutes` when a job omits the setting (#584).
pub const DEFAULT_JOB_TIMEOUT_MINUTES: u64 = 360;

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

/// Which CI system produced a project's base CI state (#591).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum BaseCiSource {
    #[default]
    GitHub,
    Rosen,
}

/// Resolves which CI system Velocity follows for a Rosen project's base (#591, #613).
///
/// Projects whose `[mirror] remote` points to Collapse always use Rosen's per-platform
/// checkpoints, whatever the issue host, because GitHub's copy is not the mirror target.
pub fn effective_base_ci_source(
    has_collapse_mirror: bool,
    issue_host: crate::collapse::IssueHost,
    saved_ci_source: Option<BaseCiSource>,
    has_github_remote: bool,
) -> BaseCiSource {
    if has_collapse_mirror
        || issue_host == crate::collapse::IssueHost::Collapse
        || !has_github_remote
    {
        return BaseCiSource::Rosen;
    }
    saved_ci_source.unwrap_or(BaseCiSource::GitHub)
}

/// One failing workflow job persisted on [`BaseCi`] so project health pairs each open
/// repair with its own workflow and job's failing commit (#697).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseCiFailure {
    /// Stable workflow identity key (#653). Empty falls back to `workflow`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workflow_key: String,
    /// Workflow display name, commit status context, or `<app> · <check>`.
    #[serde(default)]
    pub workflow: String,
    /// Failing job name (or [`WHOLE_RUN`]).
    #[serde(default)]
    pub job: String,
    /// Base commit of this job's newest failing run.
    #[serde(default)]
    pub commit: String,
    /// GitHub run ID or Rosen session ID of this failing run (0 for statuses or check runs).
    #[serde(default)]
    pub run: u64,
    /// Whether a newer run of the same workflow already has this job queued or running (#587).
    #[serde(default, skip_serializing_if = "is_false")]
    pub waiting: bool,
}

impl BaseCiFailure {
    /// Stable workflow identity key, falling back to the display name when unset (#653).
    pub fn identity(&self) -> &str {
        effective_workflow_key(&self.workflow_key, &self.workflow)
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The CI state of a project's base on its GitHub mirror or Rosen per-platform post-land
/// checks, kept with the project so it shows at startup before the next read (#507, #591).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct BaseCi {
    #[serde(default)]
    pub source: BaseCiSource,
    #[serde(default)]
    pub state: BaseCiState,
    /// The base commit of the newest finished run.
    #[serde(default)]
    pub commit: Option<String>,
    /// The failing jobs as `<workflow> · <job>`.
    #[serde(default)]
    pub failing: Vec<String>,
    /// Per-job failing run details corresponding to `failing` (#697). Saved state and backend
    /// health only; the frontend reads `failing` and `commit`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[cfg_attr(feature = "bindings", specta(skip))]
    pub failures: Vec<BaseCiFailure>,
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

/// A run's initial state from GitHub's `status` and `conclusion`. A newer push cancels the
/// run queued behind a running one, so a cancelled run starts `Undecided` and is promoted
/// to `Failed` only when its jobs show a timeout (#584).
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

/// Whether a job with this conclusion failed directly. A job concluded `cancelled` may still
/// have timed out and is classified separately via [`cancelled_job_timed_out`] (#584).
pub fn job_failed(conclusion: Option<&str>) -> bool {
    matches!(conclusion, Some("failure" | "timed_out"))
}

/// Whether a job concluded `timed_out` directly.
pub fn job_timed_out_directly(conclusion: Option<&str>) -> bool {
    conclusion == Some("timed_out")
}

/// What one job in a workflow run says about its commit (#587).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Passed,
    Failed,
    /// Queued, waiting, or in progress.
    Running,
    /// Cancelled (without timing out), skipped, or neutral: says nothing about its commit.
    Undecided,
}

/// Classifies a GitHub Actions job from its `status`, `conclusion`, and whether a `cancelled`
/// conclusion was identified as a timeout (#587).
pub fn job_state(
    status: Option<&str>,
    conclusion: Option<&str>,
    timed_out_cancelled: bool,
) -> JobState {
    if matches!(status, Some(state) if state != "completed")
        || (status.is_none() && conclusion.is_none())
    {
        return JobState::Running;
    }
    if timed_out_cancelled || job_failed(conclusion) {
        return JobState::Failed;
    }
    match conclusion {
        Some("success") => JobState::Passed,
        _ => JobState::Undecided,
    }
}

/// One job observed inside a workflow run (#587).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunJob {
    Passed(String),
    Failed(FailedJob),
    Running(String),
    Undecided(String),
}

/// Observed GitHub Actions job outcome with its runner labels for platform deploy gating (#685).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobPlatformOutcome {
    pub job: String,
    pub state: JobState,
    /// Runner labels from GitHub Actions (for example `["ubuntu-latest"]`).
    pub labels: Vec<String>,
}

impl RunJob {
    pub fn name(&self) -> &str {
        match self {
            Self::Passed(name) | Self::Running(name) | Self::Undecided(name) => name,
            Self::Failed(job) => &job.job,
        }
    }

    pub fn state(&self) -> JobState {
        match self {
            Self::Passed(_) => JobState::Passed,
            Self::Failed(_) => JobState::Failed,
            Self::Running(_) => JobState::Running,
            Self::Undecided(_) => JobState::Undecided,
        }
    }
}

/// Whether a check-run annotation message reports that a job hit its execution timeout (#584).
pub fn is_timeout_annotation(message: &str) -> bool {
    message
        .to_ascii_lowercase()
        .contains(TIMEOUT_ANNOTATION_NEEDLE)
}

/// Evidence used to decide whether a job concluded `cancelled` actually hit its timeout (#584).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CancelledJobEvidence<'a> {
    /// Annotation texts read from the job's check run, or `None` when annotations could not
    /// be read.
    pub annotations: Option<&'a [&'a str]>,
    /// Whether any newer run of the same workflow exists on the base branch.
    pub has_newer_workflow_run: bool,
    /// How long the job ran in seconds (`completed_at - started_at`).
    pub elapsed_seconds: Option<u64>,
    /// The job's `timeout-minutes`, read from the workflow file at the run's commit.
    pub timeout_minutes: Option<u64>,
}

/// Whether a job concluded `cancelled` actually failed by hitting `timeout-minutes` (#584).
///
/// GitHub reports timed-out jobs as `cancelled`. When annotations are available, a timeout
/// annotation decides. When annotations cannot be read, the job counts as timed out only
/// when no newer run of the same workflow exists on the branch and it ran at least as long
/// as its `timeout-minutes`.
pub fn cancelled_job_timed_out(evidence: &CancelledJobEvidence<'_>) -> bool {
    if let Some(annotations) = evidence.annotations {
        return annotations
            .iter()
            .any(|message| is_timeout_annotation(message));
    }
    if evidence.has_newer_workflow_run {
        return false;
    }
    let (Some(elapsed), Some(timeout_minutes)) =
        (evidence.elapsed_seconds, evidence.timeout_minutes)
    else {
        return false;
    };
    timeout_minutes > 0 && elapsed >= timeout_minutes.saturating_mul(60)
}

/// Minimal run summary used to select cancelled runs that may have timed out (#584).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunCandidateInput<'a> {
    pub workflow: &'a str,
    pub run: u64,
    pub state: RunState,
    pub cancelled: bool,
}

/// A cancelled run newer than its workflow's latest decided run (#584).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelledCandidate {
    pub workflow: String,
    pub run: u64,
    pub has_newer_workflow_run: bool,
}

/// Returns cancelled runs newer than each workflow's latest passed or failed run, ordered
/// newest-first within each workflow so callers can stop at the first confirmed timeout (#584).
pub fn cancelled_candidates(runs: &[RunCandidateInput<'_>]) -> Vec<CancelledCandidate> {
    let mut newest_decided: BTreeMap<&str, u64> = BTreeMap::new();
    let mut newest_any: BTreeMap<&str, u64> = BTreeMap::new();
    for entry in runs.iter().filter(|entry| entry.run != 0) {
        let latest = newest_any.entry(entry.workflow).or_insert(entry.run);
        if entry.run > *latest {
            *latest = entry.run;
        }
        if matches!(entry.state, RunState::Passed | RunState::Failed) {
            let decided = newest_decided.entry(entry.workflow).or_insert(entry.run);
            if entry.run > *decided {
                *decided = entry.run;
            }
        }
    }
    let mut by_workflow: BTreeMap<&str, Vec<CancelledCandidate>> = BTreeMap::new();
    for entry in runs
        .iter()
        .filter(|entry| entry.run != 0 && entry.cancelled)
    {
        if newest_decided
            .get(entry.workflow)
            .is_some_and(|decided| entry.run <= *decided)
        {
            continue;
        }
        let has_newer = newest_any
            .get(entry.workflow)
            .is_some_and(|latest| entry.run < *latest);
        by_workflow
            .entry(entry.workflow)
            .or_default()
            .push(CancelledCandidate {
                workflow: entry.workflow.to_owned(),
                run: entry.run,
                has_newer_workflow_run: has_newer,
            });
    }
    let mut candidates = Vec::new();
    for mut workflow_runs in by_workflow.into_values() {
        workflow_runs.sort_by_key(|candidate| std::cmp::Reverse(candidate.run));
        candidates.extend(workflow_runs);
    }
    candidates
}

/// One workflow run on the base branch, or one commit status or check run on its tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseRun {
    /// Stable workflow identity used for grouping and repair matching (#653).
    /// Empty falls back to `workflow` (e.g. commit statuses, check runs, or legacy fixtures).
    pub workflow_key: String,
    /// The workflow's display name, the status's context, or `<app> · <check>` for a check run.
    pub workflow: String,
    /// GitHub's run ID, which grows with each new run. A status or standalone check run has 0.
    pub run: u64,
    pub commit: String,
    pub url: String,
    pub state: RunState,
    /// What a status or check run says about itself. An Actions run's jobs and log say it instead.
    pub detail: String,
}

impl BaseRun {
    /// Stable workflow identity key, falling back to the display name when unset (#653).
    pub fn identity(&self) -> &str {
        effective_workflow_key(&self.workflow_key, &self.workflow)
    }
}

/// Returns the trimmed `workflow_key` when non-empty, or falls back to `workflow` (#653).
pub fn effective_workflow_key<'a>(workflow_key: &'a str, workflow: &'a str) -> &'a str {
    let trimmed = workflow_key.trim();
    if trimmed.is_empty() {
        workflow
    } else {
        trimmed
    }
}

/// Human-readable display name for a GitHub Actions workflow run (#653).
pub fn actions_workflow_name(name: Option<&str>, path: Option<&str>) -> String {
    name.map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| path.map(str::trim).filter(|value| !value.is_empty()))
        .unwrap_or("workflow")
        .to_owned()
}

/// Stable workflow identity key for a GitHub Actions workflow run (#653).
///
/// Prefers GitHub's immutable `workflow_id`, then the workflow file `path`, and only falls
/// back to the display name when neither is present so distinct workflows sharing a name
/// stay independent.
pub fn actions_workflow_key(
    workflow_id: Option<u64>,
    path: Option<&str>,
    display_name: &str,
) -> String {
    if let Some(id) = workflow_id.filter(|id| *id > 0) {
        return format!("workflow:{id}");
    }
    if let Some(path) = path.map(str::trim).filter(|value| !value.is_empty()) {
        return format!("path:{path}");
    }
    display_name.to_owned()
}

/// Whether a persisted or open repair matches a workflow identified by `(candidate_key, candidate_workflow)` (#653).
///
/// Repairs carrying a non-empty `repair_key` match strictly on the stable identity key so
/// distinct workflows sharing a display name never collide. Legacy repairs persisted before
/// #653 (empty `repair_key`) fall back to matching the display name.
pub fn workflow_matches_repair(
    repair_key: &str,
    repair_workflow: &str,
    candidate_key: &str,
    candidate_workflow: &str,
) -> bool {
    let repair_key = repair_key.trim();
    let candidate_key = effective_workflow_key(candidate_key, candidate_workflow);
    if !repair_key.is_empty() {
        repair_key == candidate_key
    } else {
        repair_workflow == candidate_key || repair_workflow == candidate_workflow
    }
}

/// One GitHub check run on the base branch's tip commit (#652).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckRunInput {
    pub id: u64,
    pub name: String,
    pub head_sha: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub app_id: Option<u64>,
    pub app_slug: Option<String>,
    pub app_name: Option<String>,
    pub html_url: Option<String>,
    pub details_url: Option<String>,
    pub output_title: Option<String>,
    pub output_summary: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// Whether `check` is a check-run record published by GitHub Actions itself (#652).
///
/// GitHub Actions jobs already arrive through `/actions/runs` and `/actions/runs/{id}/jobs`,
/// so skipping their check-run records prevents duplicate failures and keeps dispatched or
/// pull-request Actions runs out of base CI.
pub fn is_actions_check_run(check: &CheckRunInput, actions_job_ids: &BTreeSet<u64>) -> bool {
    if check.id != 0 && actions_job_ids.contains(&check.id) {
        return true;
    }
    if let Some(slug) = check
        .app_slug
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if slug.eq_ignore_ascii_case(GITHUB_ACTIONS_APP_SLUG) {
            return true;
        }
    } else if check.app_id == Some(GITHUB_ACTIONS_APP_ID) {
        return true;
    }
    check
        .html_url
        .as_deref()
        .is_some_and(|url| url.contains("/actions/runs/"))
}

/// Stable workflow key for a standalone GitHub App check run, scoped to its app and name (#652).
pub fn check_run_workflow(check: &CheckRunInput) -> Option<String> {
    let name = check.name.trim();
    if name.is_empty() {
        return None;
    }
    match check_run_app_label(check) {
        Some(app) if !app.eq_ignore_ascii_case(name) => Some(format!("{app} · {name}")),
        _ => Some(name.to_owned()),
    }
}

fn check_run_app_label(check: &CheckRunInput) -> Option<String> {
    if let Some(slug) = check
        .app_slug
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(slug.to_owned());
    }
    if let Some(name) = check
        .app_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(name.to_owned());
    }
    check.app_id.map(|id| format!("app-{id}"))
}

fn check_run_detail(check: &CheckRunInput) -> String {
    let title = check
        .output_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let summary = check
        .output_summary
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (title, summary) {
        (Some(title), Some(summary)) if title != summary => format!("{title}: {summary}"),
        (Some(title), _) => title.to_owned(),
        (None, Some(summary)) => summary.to_owned(),
        (None, None) => String::new(),
    }
}

fn check_run_order_key(check: &CheckRunInput, index: usize) -> (u64, &str, usize) {
    let timestamp = check
        .completed_at
        .as_deref()
        .or(check.started_at.as_deref())
        .unwrap_or("");
    (check.id, timestamp, index)
}

/// Converts standalone GitHub App check runs on the base tip into [`BaseRun`] records (#652).
///
/// GitHub Actions check runs are omitted because `/actions/runs` already tracks them. When
/// multiple check runs share the same `(app, name)`, only the newest check run is kept.
pub fn check_runs_to_base_runs(
    checks: &[CheckRunInput],
    tip_sha: &str,
    commit_page: &str,
    actions_job_ids: &BTreeSet<u64>,
) -> Vec<BaseRun> {
    let mut latest: BTreeMap<String, (usize, &CheckRunInput)> = BTreeMap::new();
    for (index, check) in checks.iter().enumerate() {
        if is_actions_check_run(check, actions_job_ids) {
            continue;
        }
        let Some(workflow) = check_run_workflow(check) else {
            continue;
        };
        match latest.entry(workflow) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((index, check));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let (current_index, current) = *entry.get();
                if check_run_order_key(check, index) > check_run_order_key(current, current_index) {
                    entry.insert((index, check));
                }
            }
        }
    }
    latest
        .into_iter()
        .map(|(workflow, (_, check))| {
            let commit = if check.head_sha.trim().is_empty() {
                tip_sha.to_owned()
            } else {
                check.head_sha.clone()
            };
            let url = check
                .html_url
                .as_deref()
                .or(check.details_url.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(commit_page)
                .to_owned();
            BaseRun {
                workflow_key: workflow.clone(),
                workflow,
                run: 0,
                commit,
                url,
                state: run_state(check.status.as_deref(), check.conclusion.as_deref()),
                detail: check_run_detail(check),
            }
        })
        .collect()
}

/// A job that failed, with the first step that failed in it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailedJob {
    /// GitHub's job (check-run) ID; 0 for commit statuses or whole-run failures (#584).
    pub id: u64,
    pub job: String,
    pub step: Option<String>,
    /// The runner labels, such as `macos-latest`.
    pub runner: Option<String>,
    /// Normalized signature of the first concrete error in this job's failed log (#558).
    pub signature: Option<String>,
    /// Whether this job failed by exceeding its timeout (including GitHub reporting a timeout
    /// as `cancelled`) (#584).
    pub timed_out: bool,
    /// The test still running when a timed-out job stopped, from the last
    /// `has been running for over 60 seconds` line of its log (#584).
    pub hung_test: Option<String>,
}

/// A workflow whose newest finished run failed, with the jobs that failed in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// Stable workflow identity used for repair matching (#653).
    pub workflow_key: String,
    /// Human-readable workflow display name shown in UI, thread titles, and prompts.
    pub workflow: String,
    pub run: u64,
    pub commit: String,
    pub url: String,
    pub detail: String,
    pub jobs: Vec<FailedJob>,
}

impl Failure {
    /// Stable workflow identity key, falling back to the display name when unset (#653).
    pub fn identity(&self) -> &str {
        effective_workflow_key(&self.workflow_key, &self.workflow)
    }
}

/// What the newest finished runs say, before any failed job is read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Stable identities of workflows whose newest finished run passed.
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
        let known = newest.entry(run.identity()).or_insert(run);
        if run.run > known.run {
            *known = run;
        }
    }
    let running = runs.iter().any(|run| {
        run.state == RunState::Running
            && newest
                .get(run.identity())
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
        passed: passed
            .into_iter()
            .map(|run| run.identity().to_owned())
            .collect(),
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
        workflow_key: run.identity().to_owned(),
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
    /// Stable workflow identity key (#653). Empty on legacy repairs persisted before #653.
    pub workflow_key: String,
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

impl OpenRepair {
    /// Stable workflow identity key, falling back to the display name for legacy repairs (#653).
    pub fn identity(&self) -> &str {
        effective_workflow_key(&self.workflow_key, &self.workflow)
    }
}

/// An open repair superseded by a failure at or after its land, or a new failure after
/// its thread ended (#558).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SupersededRepair {
    /// Stable workflow identity key (#653).
    pub workflow_key: String,
    pub workflow: String,
    pub job: String,
    pub thread: u32,
    pub commit: String,
    pub landed_commit: Option<String>,
}

impl SupersededRepair {
    /// Stable workflow identity key, falling back to the display name when unset (#653).
    pub fn identity(&self) -> &str {
        effective_workflow_key(&self.workflow_key, &self.workflow)
    }
}

/// A job whose newest finished outcome is a pass (#587).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedJob {
    /// Stable workflow identity key (#653).
    pub workflow_key: String,
    pub workflow: String,
    pub job: String,
    pub commit: String,
    pub run: u64,
}

impl ResolvedJob {
    /// Stable workflow identity key, falling back to the display name when unset (#653).
    pub fn identity(&self) -> &str {
        effective_workflow_key(&self.workflow_key, &self.workflow)
    }
}

/// A workflow's newest completed passing run (#587).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PassedWorkflow {
    /// Stable workflow identity key (#653).
    pub workflow_key: String,
    pub workflow: String,
    pub commit: String,
    pub run: u64,
}

impl PassedWorkflow {
    /// Stable workflow identity key, falling back to the display name when unset (#653).
    pub fn identity(&self) -> &str {
        effective_workflow_key(&self.workflow_key, &self.workflow)
    }
}

/// Per-job evaluation across finished and in-progress runs of every workflow (#587).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobVerdict {
    /// Workflows whose newest deciding run passed and that have no failing job in any
    /// newer in-progress run.
    pub passed: Vec<String>,
    /// Newest whole-workflow passing run for each workflow that has one.
    pub passed_workflows: Vec<PassedWorkflow>,
    /// Jobs whose newest finished outcome passed (including in-progress or cancelled runs).
    pub resolved_jobs: Vec<ResolvedJob>,
    /// Failing jobs whose newest finished run failed and that have no newer run currently
    /// queued or running the same job, grouped by `(workflow, run)`.
    pub active_failures: Vec<Failure>,
    /// Failing jobs whose newest finished run failed while a newer run still has the same
    /// job queued or running (shown as failing, but wait before starting or reopening a repair).
    pub waiting_failures: Vec<Failure>,
    /// A run newer than or without a finished workflow run is queued or in progress.
    pub running: bool,
    /// Commit of the newest failing run when any job is failing, or else the newest finished run.
    pub commit: Option<String>,
}

/// Runs whose jobs should be read so each job is decided by the newest run in which it
/// finished (#587): each workflow's newest failed run, plus up to
/// `max_running_per_workflow` running runs newer than its newest decided run.
pub fn runs_to_inspect(runs: &[BaseRun], max_running_per_workflow: usize) -> Vec<u64> {
    runs_to_inspect_for_commits(runs, max_running_per_workflow, false, &BTreeSet::new())
}

/// Like [`runs_to_inspect`], optionally also inspecting each workflow's newest passing run
/// and any run matching `commits` so platform-gated deploys can read runner labels (#685).
pub fn runs_to_inspect_for_commits(
    runs: &[BaseRun],
    max_running_per_workflow: usize,
    include_newest_passed: bool,
    commits: &BTreeSet<String>,
) -> Vec<u64> {
    let mut by_workflow: BTreeMap<&str, Vec<&BaseRun>> = BTreeMap::new();
    for run in runs.iter().filter(|run| run.run != 0) {
        by_workflow.entry(run.identity()).or_default().push(run);
    }
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push_unique = |run_id: u64| {
        if seen.insert(run_id) {
            selected.push(run_id);
        }
    };
    for mut workflow_runs in by_workflow.into_values() {
        workflow_runs.sort_by_key(|run| std::cmp::Reverse(run.run));
        let newest_decided = workflow_runs
            .iter()
            .copied()
            .find(|run| matches!(run.state, RunState::Passed | RunState::Failed));
        for run in workflow_runs
            .iter()
            .copied()
            .filter(|run| {
                run.state == RunState::Running
                    && newest_decided.is_none_or(|decided| run.run > decided.run)
            })
            .take(max_running_per_workflow)
        {
            push_unique(run.run);
        }
        if let Some(failed) = newest_decided.filter(|run| run.state == RunState::Failed) {
            push_unique(failed.run);
        }
        if include_newest_passed
            && let Some(passed) = workflow_runs
                .iter()
                .copied()
                .find(|run| run.state == RunState::Passed)
        {
            push_unique(passed.run);
        }
        if !commits.is_empty() {
            for run in &workflow_runs {
                if commits
                    .iter()
                    .any(|commit| commit.eq_ignore_ascii_case(run.commit.trim()))
                {
                    push_unique(run.run);
                }
            }
        }
    }
    selected
}

/// Decides every job by the newest run in which that job finished, whether or not the rest
/// of that run has finished (#587).
pub fn evaluate_jobs(runs: &[BaseRun], run_jobs: &BTreeMap<u64, Vec<RunJob>>) -> JobVerdict {
    let mut by_workflow: BTreeMap<&str, Vec<&BaseRun>> = BTreeMap::new();
    for run in runs {
        by_workflow.entry(run.identity()).or_default().push(run);
    }
    let mut verdict = JobVerdict::default();
    for (workflow_key, workflow_runs) in by_workflow {
        evaluate_workflow(workflow_key, workflow_runs, run_jobs, &mut verdict);
    }
    verdict.commit = newest_verdict_commit(&verdict);
    verdict
}

fn evaluate_workflow(
    workflow_key: &str,
    mut runs: Vec<&BaseRun>,
    run_jobs: &BTreeMap<u64, Vec<RunJob>>,
    verdict: &mut JobVerdict,
) {
    if runs.iter().all(|run| run.run == 0) {
        if let Some(status) = runs.last().copied() {
            evaluate_status(status, verdict);
        }
        return;
    }
    runs.retain(|run| run.run != 0);
    runs.sort_by_key(|run| std::cmp::Reverse(run.run));
    let newest_decided = runs
        .iter()
        .copied()
        .find(|run| matches!(run.state, RunState::Passed | RunState::Failed));
    let newest_passed = runs
        .iter()
        .copied()
        .find(|run| run.state == RunState::Passed);
    if runs.iter().any(|run| {
        run.state == RunState::Running
            && newest_decided.is_none_or(|decided| run.run >= decided.run)
    }) {
        verdict.running = true;
    }
    let relevant: Vec<&BaseRun> = runs
        .iter()
        .copied()
        .filter(|run| newest_decided.is_none_or(|decided| run.run >= decided.run))
        .collect();
    let has_failure = record_workflow_jobs(workflow_key, &relevant, run_jobs, verdict);
    if let Some(passed_run) = newest_passed {
        verdict.passed_workflows.push(PassedWorkflow {
            workflow_key: workflow_key.to_owned(),
            workflow: passed_run.workflow.clone(),
            commit: passed_run.commit.clone(),
            run: passed_run.run,
        });
        if !has_failure && newest_decided.is_some_and(|decided| decided.run == passed_run.run) {
            verdict.passed.push(workflow_key.to_owned());
        }
    }
}

fn record_workflow_jobs(
    workflow_key: &str,
    relevant: &[&BaseRun],
    run_jobs: &BTreeMap<u64, Vec<RunJob>>,
    verdict: &mut JobVerdict,
) -> bool {
    let mut active_by_run: BTreeMap<u64, (&BaseRun, Vec<FailedJob>)> = BTreeMap::new();
    let mut waiting_by_run: BTreeMap<u64, (&BaseRun, Vec<FailedJob>)> = BTreeMap::new();
    for job_name in candidate_job_names(relevant, run_jobs) {
        match decide_job(&job_name, relevant, run_jobs) {
            JobDecision::Passed {
                workflow,
                commit,
                run,
            } => verdict.resolved_jobs.push(ResolvedJob {
                workflow_key: workflow_key.to_owned(),
                workflow,
                job: job_name,
                commit,
                run,
            }),
            JobDecision::Failed {
                run,
                job,
                waiting: false,
            } => push_grouped_failure(&mut active_by_run, run, job),
            JobDecision::Failed {
                run,
                job,
                waiting: true,
            } => push_grouped_failure(&mut waiting_by_run, run, job),
            JobDecision::Undecided => {}
        }
    }
    let has_failure = !active_by_run.is_empty() || !waiting_by_run.is_empty();
    for (_, (run, jobs)) in active_by_run.into_iter().rev() {
        verdict.active_failures.push(failure(run, jobs));
    }
    for (_, (run, jobs)) in waiting_by_run.into_iter().rev() {
        verdict.waiting_failures.push(failure(run, jobs));
    }
    has_failure
}

fn evaluate_status(status: &BaseRun, verdict: &mut JobVerdict) {
    match status.state {
        RunState::Passed => {
            verdict.passed.push(status.identity().to_owned());
            verdict.passed_workflows.push(PassedWorkflow {
                workflow_key: status.identity().to_owned(),
                workflow: status.workflow.clone(),
                commit: status.commit.clone(),
                run: 0,
            });
        }
        RunState::Failed => verdict.active_failures.push(failure(status, Vec::new())),
        RunState::Running => verdict.running = true,
        RunState::Undecided => {}
    }
}

fn candidate_job_names(
    relevant_runs: &[&BaseRun],
    run_jobs: &BTreeMap<u64, Vec<RunJob>>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for run in relevant_runs {
        if let Some(jobs) = run_jobs.get(&run.run) {
            for job in jobs {
                names.insert(job.name().to_owned());
            }
            if run.state == RunState::Failed && !run_has_failed_job(jobs) {
                names.insert(WHOLE_RUN.to_owned());
            }
        } else if run.state == RunState::Failed {
            names.insert(WHOLE_RUN.to_owned());
        }
    }
    names
}

fn run_has_failed_job(jobs: &[RunJob]) -> bool {
    jobs.iter().any(|job| job.state() == JobState::Failed)
}

enum JobInRun {
    Passed,
    Failed(FailedJob),
    Running,
    Undecided,
}

enum JobDecision<'a> {
    Passed {
        workflow: String,
        commit: String,
        run: u64,
    },
    Failed {
        run: &'a BaseRun,
        job: FailedJob,
        waiting: bool,
    },
    Undecided,
}

fn decide_job<'a>(
    job_name: &str,
    relevant_runs: &[&'a BaseRun],
    run_jobs: &BTreeMap<u64, Vec<RunJob>>,
) -> JobDecision<'a> {
    let mut pending_newer_run = false;
    for &run in relevant_runs {
        match job_outcome_in_run(job_name, run, run_jobs) {
            JobInRun::Passed => {
                return JobDecision::Passed {
                    workflow: run.workflow.clone(),
                    commit: run.commit.clone(),
                    run: run.run,
                };
            }
            JobInRun::Failed(job) => {
                return JobDecision::Failed {
                    run,
                    job,
                    waiting: pending_newer_run,
                };
            }
            JobInRun::Running => pending_newer_run = true,
            JobInRun::Undecided => {}
        }
    }
    JobDecision::Undecided
}

fn job_outcome_in_run(
    job_name: &str,
    run: &BaseRun,
    run_jobs: &BTreeMap<u64, Vec<RunJob>>,
) -> JobInRun {
    if let Some(jobs) = run_jobs.get(&run.run) {
        let mut matched = jobs.iter().filter(|entry| entry.name() == job_name);
        if let Some(first) = matched.next() {
            return strongest_job_outcome(std::iter::once(first).chain(matched));
        }
        if job_name == WHOLE_RUN && run.state == RunState::Failed && !run_has_failed_job(jobs) {
            return JobInRun::Failed(FailedJob {
                job: WHOLE_RUN.into(),
                ..FailedJob::default()
            });
        }
        return match run.state {
            RunState::Passed => JobInRun::Passed,
            RunState::Running => JobInRun::Running,
            RunState::Failed | RunState::Undecided => JobInRun::Undecided,
        };
    }
    match run.state {
        RunState::Passed => JobInRun::Passed,
        RunState::Running => JobInRun::Running,
        RunState::Failed if job_name == WHOLE_RUN => JobInRun::Failed(FailedJob {
            job: WHOLE_RUN.into(),
            ..FailedJob::default()
        }),
        RunState::Failed | RunState::Undecided => JobInRun::Undecided,
    }
}

fn strongest_job_outcome<'a>(entries: impl Iterator<Item = &'a RunJob>) -> JobInRun {
    let mut outcome = JobInRun::Undecided;
    for entry in entries {
        match entry {
            RunJob::Failed(job) => return JobInRun::Failed(job.clone()),
            RunJob::Running(_) => outcome = JobInRun::Running,
            RunJob::Passed(_) if !matches!(outcome, JobInRun::Running) => {
                outcome = JobInRun::Passed;
            }
            RunJob::Passed(_) | RunJob::Undecided(_) => {}
        }
    }
    outcome
}

fn push_grouped_failure<'a>(
    grouped: &mut BTreeMap<u64, (&'a BaseRun, Vec<FailedJob>)>,
    run: &'a BaseRun,
    job: FailedJob,
) {
    grouped
        .entry(run.run)
        .or_insert_with(|| (run, Vec::new()))
        .1
        .push(job);
}

fn newest_verdict_commit(verdict: &JobVerdict) -> Option<String> {
    let failing_commit = verdict
        .active_failures
        .iter()
        .chain(&verdict.waiting_failures)
        .max_by_key(|failure| failure.run)
        .map(|failure| failure.commit.clone());
    if failing_commit.is_some() {
        return failing_commit;
    }
    verdict
        .passed_workflows
        .iter()
        .max_by_key(|workflow| workflow.run)
        .map(|workflow| workflow.commit.clone())
        .or_else(|| {
            verdict
                .resolved_jobs
                .iter()
                .max_by_key(|job| job.run)
                .map(|job| job.commit.clone())
        })
}

/// What a look changes: workflows whose repairs a green run closes, individual jobs resolved
/// by a newer passing job, open repairs superseded by a failure at or after their land, and
/// failures needing a new thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub closed: Vec<String>,
    pub resolved: Vec<ResolvedJob>,
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
    let verdict = JobVerdict {
        passed: passed.to_vec(),
        active_failures: failures.to_vec(),
        ..JobVerdict::default()
    };
    plan_for_verdict(open, &verdict)
}

/// Plans repair closures, individual resolved-job closures, reopens and new threads from a
/// per-job [`JobVerdict`] (#558, #587, #653).
pub fn plan_for_verdict(open: &[OpenRepair], verdict: &JobVerdict) -> Plan {
    let closed = collect_closed_workflows(open, verdict);
    let resolved = collect_resolved_repairs(open, verdict, &closed);
    let mut superseded = Vec::new();
    for repair in open {
        if repair_is_closed(repair, &closed, &resolved)
            || repair_is_waiting(repair, &verdict.waiting_failures)
        {
            continue;
        }
        if repair_is_superseded_by_failures(repair, &verdict.active_failures) {
            superseded.push(SupersededRepair {
                workflow_key: repair.identity().to_owned(),
                workflow: repair.workflow.clone(),
                job: repair.job.clone(),
                thread: repair.thread,
                commit: repair.commit.clone(),
                landed_commit: repair.landed_commit.clone(),
            });
        }
    }
    let start = verdict
        .active_failures
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
        resolved,
        superseded,
        start,
    }
}

fn collect_closed_workflows(open: &[OpenRepair], verdict: &JobVerdict) -> Vec<String> {
    let mut closed = Vec::new();
    for passed_key in &verdict.passed {
        let passed_display = verdict
            .passed_workflows
            .iter()
            .find(|item| item.identity() == passed_key)
            .map_or(passed_key.as_str(), |item| item.workflow.as_str());
        for repair in open {
            let matches = if !repair.workflow_key.trim().is_empty() {
                repair.identity() == passed_key
            } else {
                !display_workflow_is_failing(&repair.workflow, verdict)
                    && (repair.workflow == *passed_key || repair.workflow == passed_display)
            };
            if matches {
                let key = repair.identity().to_owned();
                if !closed.contains(&key) {
                    closed.push(key);
                }
            }
        }
    }
    closed
}

fn display_workflow_is_failing(workflow: &str, verdict: &JobVerdict) -> bool {
    verdict
        .active_failures
        .iter()
        .chain(&verdict.waiting_failures)
        .any(|failure| failure.workflow == workflow)
}

fn legacy_repair_blocked_by_display_failure(repair: &OpenRepair, verdict: &JobVerdict) -> bool {
    repair.workflow_key.trim().is_empty()
        && verdict
            .active_failures
            .iter()
            .chain(&verdict.waiting_failures)
            .any(|failure| {
                failure.workflow == repair.workflow
                    && failure.jobs.iter().any(|job| job.job == repair.job)
            })
}

fn collect_resolved_repairs(
    open: &[OpenRepair],
    verdict: &JobVerdict,
    closed: &[String],
) -> Vec<ResolvedJob> {
    let mut resolved: Vec<ResolvedJob> = Vec::new();
    for repair in open {
        if closed.iter().any(|key| key == repair.identity())
            || legacy_repair_blocked_by_display_failure(repair, verdict)
        {
            continue;
        }
        let candidate = verdict
            .resolved_jobs
            .iter()
            .find(|item| {
                workflow_matches_repair(
                    &repair.workflow_key,
                    &repair.workflow,
                    item.identity(),
                    &item.workflow,
                ) && item.job == repair.job
                    && (item.run == 0 || repair.run == 0 || item.run >= repair.run)
            })
            .cloned()
            .or_else(|| resolved_by_passed_workflow(repair, verdict));
        let Some(item) = candidate else {
            continue;
        };
        if !resolved
            .iter()
            .any(|existing| existing.identity() == item.identity() && existing.job == item.job)
        {
            resolved.push(item);
        }
    }
    resolved
}

fn resolved_by_passed_workflow(repair: &OpenRepair, verdict: &JobVerdict) -> Option<ResolvedJob> {
    let passed = verdict.passed_workflows.iter().find(|workflow| {
        workflow_matches_repair(
            &repair.workflow_key,
            &repair.workflow,
            workflow.identity(),
            &workflow.workflow,
        ) && (workflow.run == 0 || repair.run == 0 || workflow.run >= repair.run)
    })?;
    let failed_after = verdict
        .active_failures
        .iter()
        .chain(&verdict.waiting_failures)
        .any(|failure| {
            failure.identity() == passed.identity()
                && failure.run > passed.run
                && failure.jobs.iter().any(|job| job.job == repair.job)
        });
    (!failed_after).then(|| ResolvedJob {
        workflow_key: passed.identity().to_owned(),
        workflow: passed.workflow.clone(),
        job: repair.job.clone(),
        commit: passed.commit.clone(),
        run: passed.run,
    })
}

fn repair_is_closed(repair: &OpenRepair, closed: &[String], resolved: &[ResolvedJob]) -> bool {
    closed.iter().any(|key| key == repair.identity())
        || resolved.iter().any(|item| {
            workflow_matches_repair(
                &repair.workflow_key,
                &repair.workflow,
                item.identity(),
                &item.workflow,
            ) && item.job == repair.job
                && (item.run == 0 || repair.run == 0 || item.run >= repair.run)
        })
}

fn repair_is_waiting(repair: &OpenRepair, waiting_failures: &[Failure]) -> bool {
    waiting_failures.iter().any(|failure| {
        workflow_matches_repair(
            &repair.workflow_key,
            &repair.workflow,
            failure.identity(),
            &failure.workflow,
        ) && failure.jobs.iter().any(|job| job.job == repair.job)
    })
}

fn repair_is_superseded_by_failures(repair: &OpenRepair, active_failures: &[Failure]) -> bool {
    if let Some((failure, job)) = active_failures.iter().find_map(|failure| {
        workflow_matches_repair(
            &repair.workflow_key,
            &repair.workflow,
            failure.identity(),
            &failure.workflow,
        )
        .then(|| failure.jobs.iter().find(|job| job.job == repair.job))
        .flatten()
        .map(|job| (failure, job))
    }) {
        return is_repair_superseded(repair, failure, job);
    }
    active_failures
        .iter()
        .filter(|failure| {
            workflow_matches_repair(
                &repair.workflow_key,
                &repair.workflow,
                failure.identity(),
                &failure.workflow,
            )
        })
        .max_by_key(|failure| failure.run)
        .is_some_and(|failure| {
            repair.thread_state == RepairThreadState::Landed
                && effective_commit_order(repair, &failure.commit) == CommitOrder::AtOrAfterLand
        })
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
    if !workflow_matches_repair(
        &repair.workflow_key,
        &repair.workflow,
        failure.identity(),
        &failure.workflow,
    ) || repair.job != job.job
    {
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

/// Formats the project health action line for a red base CI check, naming the covered
/// commit and whether the latest failure is at or newer than the repair's land (#558, #591).
pub fn format_ci_repair_action(
    repair: Option<&OpenRepair>,
    failing_commit: Option<&str>,
    spec_locked: bool,
) -> String {
    format_ci_repair_action_for_source(repair, failing_commit, spec_locked, BaseCiSource::GitHub)
}

/// Formats the project health action line for either GitHub CI or Rosen post-land checks (#591).
pub fn format_ci_repair_action_for_source(
    repair: Option<&OpenRepair>,
    failing_commit: Option<&str>,
    spec_locked: bool,
    source: BaseCiSource,
) -> String {
    let Some(repair) = repair else {
        return if !spec_locked {
            "Cannot start fix-forward thread: project spec is not locked".into()
        } else {
            match source {
                BaseCiSource::GitHub => "Queued fix-forward thread on next GitHub CI check".into(),
                BaseCiSource::Rosen => {
                    "Queued fix-forward thread on next Rosen post-land check".into()
                }
            }
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

/// The failure record inside [`BaseCi`] paired with one open CI repair (#697).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchedBaseCiFailure<'a> {
    /// Base commit of this repair's own workflow and job failure.
    pub commit: &'a str,
    /// True when a newer run of the same workflow already has this job queued or running (#587).
    pub waiting: bool,
}

/// Minimal repair metadata used to choose which open repair project health displays (#697).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepairCandidate<'a> {
    pub workflow_key: &'a str,
    pub workflow: &'a str,
    pub job: &'a str,
    pub active: bool,
}

/// Pairs one open CI repair with its own workflow/job failure inside `ci`, mirroring the
/// poller's `matching_repair_failure` and `repair_is_waiting` rules (#697).
///
/// Never returns another workflow's failing commit. When `ci.failures` is empty (a legacy
/// `BaseCi` persisted before #697), falls back to `ci.commit` only when `ci.failing` names
/// this repair's job alone.
pub fn matching_base_ci_failure<'a>(
    ci: &'a BaseCi,
    workflow_key: &str,
    workflow: &str,
    job: &str,
) -> Option<MatchedBaseCiFailure<'a>> {
    if ci.failures.is_empty() {
        let label = job_label(workflow, job);
        if ci.failing.len() == 1 && ci.failing[0] == label {
            return ci
                .commit
                .as_deref()
                .map(str::trim)
                .filter(|commit| !commit.is_empty())
                .map(|commit| MatchedBaseCiFailure {
                    commit,
                    waiting: false,
                });
        }
        return None;
    }
    let matches_workflow = |entry: &&BaseCiFailure| {
        workflow_matches_repair(workflow_key, workflow, entry.identity(), &entry.workflow)
    };
    let matched = ci
        .failures
        .iter()
        .filter(|entry| matches_workflow(entry) && entry.job == job && entry.waiting)
        .max_by_key(|entry| entry.run)
        .or_else(|| {
            ci.failures
                .iter()
                .filter(|entry| matches_workflow(entry) && entry.job == job && !entry.waiting)
                .max_by_key(|entry| entry.run)
        })
        .or_else(|| {
            ci.failures
                .iter()
                .filter(|entry| matches_workflow(entry) && !entry.waiting)
                .max_by_key(|entry| entry.run)
        })?;
    let commit = matched.commit.trim();
    (!commit.is_empty()).then_some(MatchedBaseCiFailure {
        commit,
        waiting: matched.waiting,
    })
}

/// Returns the failing commit project health should compare and display for one repair (#697).
///
/// For a landed repair whose matching job already has a newer run queued or in progress,
/// returns `None` so health reports waiting for CI instead of claiming a replacement is queued.
pub fn repair_health_failing_commit<'a>(
    thread_state: RepairThreadState,
    matched: Option<MatchedBaseCiFailure<'a>>,
) -> Option<&'a str> {
    let matched = matched?;
    if matched.waiting && thread_state == RepairThreadState::Landed {
        return None;
    }
    Some(matched.commit)
}

/// Picks the open repair index project health should pair with `ci` (#697).
///
/// Prefers the newest active repair whose workflow/job currently fails in `ci`, then the
/// newest active repair overall, then the newest repair matching a current failure in `ci`,
/// and finally the newest repair overall.
pub fn select_ci_repair_index(candidates: &[RepairCandidate<'_>], ci: &BaseCi) -> Option<usize> {
    let matches_ci = |candidate: &RepairCandidate<'_>| {
        matching_base_ci_failure(
            ci,
            candidate.workflow_key,
            candidate.workflow,
            candidate.job,
        )
        .is_some()
    };
    candidates
        .iter()
        .enumerate()
        .rev()
        .find(|(_, candidate)| candidate.active && matches_ci(candidate))
        .or_else(|| {
            candidates
                .iter()
                .enumerate()
                .rev()
                .find(|(_, candidate)| candidate.active)
        })
        .or_else(|| {
            candidates
                .iter()
                .enumerate()
                .rev()
                .find(|(_, candidate)| matches_ci(candidate))
        })
        .map(|(index, _)| index)
        .or_else(|| candidates.len().checked_sub(1))
}

fn base_ci_failures(active: &[Failure], waiting: &[Failure]) -> Vec<BaseCiFailure> {
    let mut entries = Vec::new();
    for (failures, is_waiting) in [(active, false), (waiting, true)] {
        for failure in failures {
            for job in &failure.jobs {
                entries.push(BaseCiFailure {
                    workflow_key: failure.identity().to_owned(),
                    workflow: failure.workflow.clone(),
                    job: job.job.clone(),
                    commit: failure.commit.clone(),
                    run: failure.run,
                    waiting: is_waiting,
                });
            }
        }
    }
    entries
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
        source: BaseCiSource::GitHub,
        state,
        commit: verdict.commit.clone(),
        failing,
        failures: base_ci_failures(failures, &[]),
        running: verdict.running,
    }
}

/// What the project shows from a per-job [`JobVerdict`] (#587): any job whose newest
/// finished run failed keeps the base failing until a newer run passes that job.
pub fn view_for_verdict(verdict: &JobVerdict) -> BaseCi {
    let failing: Vec<String> = verdict
        .active_failures
        .iter()
        .chain(&verdict.waiting_failures)
        .flat_map(|failure| {
            failure
                .jobs
                .iter()
                .map(|job| job_label(&failure.workflow, &job.job))
        })
        .collect();
    let state = if !failing.is_empty() {
        BaseCiState::Failed
    } else if verdict.running {
        BaseCiState::Running
    } else if verdict.passed.is_empty() {
        BaseCiState::NotRun
    } else {
        BaseCiState::Green
    };
    BaseCi {
        source: BaseCiSource::GitHub,
        state,
        commit: verdict.commit.clone(),
        failing,
        failures: base_ci_failures(&verdict.active_failures, &verdict.waiting_failures),
        running: verdict.running,
    }
}

/// Synthetic workflow name under which Rosen per-platform post-land repairs are tracked (#591).
pub const ROSEN_BASE_WORKFLOW: &str = "Rosen post-land";

/// One configured platform's post-land checkpoints for base CI evaluation (#591).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RosenPlatformCheckpoint {
    pub platform: String,
    pub display_name: String,
    pub required_build: bool,
    pub required_test: bool,
    pub last_built: Option<String>,
    pub last_green: Option<String>,
    pub checker_connected: bool,
}

/// One confirmed Rosen post-land failure evaluated by the base CI watcher (#591).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RosenPostLandFailure {
    pub session: u64,
    pub change: String,
    pub commit: String,
    /// `"build"` or `"test"`.
    pub kind: String,
    /// Configured platform key (e.g. `"macos"`), or `None` for any-platform checks.
    pub platform: Option<String>,
    /// Human-readable platform name (e.g. `"macOS"`), or `None`.
    pub platform_display: Option<String>,
    pub output: String,
    /// True once that platform's (or the base's) green/built checkpoint covers `commit`.
    pub covered_by_green: bool,
    pub reverted: bool,
}

/// Read-only snapshot of Rosen per-platform post-land state for pure evaluation (#591).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RosenBaseSnapshot {
    pub base: Option<String>,
    pub last_green: Option<String>,
    pub platforms: Vec<RosenPlatformCheckpoint>,
    pub failures: Vec<RosenPostLandFailure>,
    pub bisecting: bool,
}

/// Stable job key for a Rosen per-platform failure (e.g. `"macOS · test"` or `"post-land test"`).
pub fn rosen_failure_job_name(platform_display: Option<&str>, kind: &str) -> String {
    match platform_display
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        Some(platform) => format!("{platform} · {kind}"),
        None => format!("post-land {kind}"),
    }
}

/// Evaluates Rosen per-platform post-land failures and checkpoints into a [`JobVerdict`]
/// and a [`BaseCi`] view without consulting GitHub Actions (#591).
pub fn evaluate_rosen_post_land(snapshot: &RosenBaseSnapshot) -> (JobVerdict, BaseCi) {
    let mut latest_by_job: BTreeMap<String, &RosenPostLandFailure> = BTreeMap::new();
    for entry in &snapshot.failures {
        if entry.reverted || entry.covered_by_green {
            continue;
        }
        let job = rosen_failure_job_name(entry.platform_display.as_deref(), &entry.kind);
        let slot = latest_by_job.entry(job).or_insert(entry);
        if entry.session >= slot.session {
            *slot = entry;
        }
    }
    let mut verdict = JobVerdict {
        running: rosen_checks_running(snapshot),
        ..JobVerdict::default()
    };
    let mut active: Vec<(&str, &RosenPostLandFailure)> = latest_by_job
        .iter()
        .map(|(job, entry)| (job.as_str(), *entry))
        .collect();
    active.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.session));
    for (job_name, entry) in active {
        verdict
            .active_failures
            .push(rosen_failure_to_ci(job_name, entry));
    }
    record_rosen_resolved_jobs(snapshot, &latest_by_job, &mut verdict);
    let view = build_rosen_base_view(snapshot, &verdict);
    verdict.commit = view.commit.clone();
    (verdict, view)
}

fn rosen_failure_to_ci(job_name: &str, entry: &RosenPostLandFailure) -> Failure {
    Failure {
        workflow_key: ROSEN_BASE_WORKFLOW.to_owned(),
        workflow: ROSEN_BASE_WORKFLOW.to_owned(),
        run: entry.session,
        commit: entry.commit.clone(),
        url: String::new(),
        detail: format!("Culprit change: {}", entry.change),
        jobs: vec![FailedJob {
            id: entry.session,
            job: job_name.to_owned(),
            step: Some(format!("post-land {}", entry.kind)),
            runner: entry.platform_display.clone(),
            signature: error_signature(&entry.output),
            timed_out: false,
            hung_test: hung_test(&entry.output),
        }],
    }
}

fn record_rosen_resolved_jobs(
    snapshot: &RosenBaseSnapshot,
    active_jobs: &BTreeMap<String, &RosenPostLandFailure>,
    verdict: &mut JobVerdict,
) {
    for platform in &snapshot.platforms {
        if let Some(green) = &platform.last_green {
            let job = rosen_failure_job_name(Some(&platform.display_name), "test");
            if !active_jobs.contains_key(&job) {
                verdict.resolved_jobs.push(ResolvedJob {
                    workflow_key: ROSEN_BASE_WORKFLOW.to_owned(),
                    workflow: ROSEN_BASE_WORKFLOW.to_owned(),
                    job,
                    commit: green.clone(),
                    run: 0,
                });
            }
        }
        if let Some(built) = platform
            .last_built
            .as_ref()
            .or(platform.last_green.as_ref())
        {
            let job = rosen_failure_job_name(Some(&platform.display_name), "build");
            if !active_jobs.contains_key(&job) {
                verdict.resolved_jobs.push(ResolvedJob {
                    workflow_key: ROSEN_BASE_WORKFLOW.to_owned(),
                    workflow: ROSEN_BASE_WORKFLOW.to_owned(),
                    job,
                    commit: built.clone(),
                    run: 0,
                });
            }
        }
    }
    if active_jobs.is_empty()
        && rosen_all_required_green(snapshot)
        && let Some(commit) = snapshot.last_green.clone()
    {
        verdict.passed.push(ROSEN_BASE_WORKFLOW.to_owned());
        verdict.passed_workflows.push(PassedWorkflow {
            workflow_key: ROSEN_BASE_WORKFLOW.to_owned(),
            workflow: ROSEN_BASE_WORKFLOW.to_owned(),
            commit,
            run: 0,
        });
    }
}

fn rosen_all_required_green(snapshot: &RosenBaseSnapshot) -> bool {
    if snapshot.platforms.is_empty() {
        return snapshot.last_green.is_some();
    }
    snapshot.platforms.iter().all(|platform| {
        let test_ok = !platform.required_test || platform.last_green.is_some();
        let build_ok = !platform.required_build
            || platform.last_built.is_some()
            || platform.last_green.is_some();
        test_ok && build_ok
    })
}

fn rosen_checks_running(snapshot: &RosenBaseSnapshot) -> bool {
    if snapshot.bisecting {
        return true;
    }
    let Some(base) = snapshot.base.as_deref() else {
        return false;
    };
    if snapshot.platforms.is_empty() {
        return snapshot
            .last_green
            .as_deref()
            .is_none_or(|green| !green.eq_ignore_ascii_case(base));
    }
    snapshot.platforms.iter().any(|platform| {
        let test_behind = platform.required_test
            && platform
                .last_green
                .as_deref()
                .is_none_or(|green| !green.eq_ignore_ascii_case(base));
        let build_behind = platform.required_build
            && platform
                .last_built
                .as_deref()
                .or(platform.last_green.as_deref())
                .is_none_or(|built| !built.eq_ignore_ascii_case(base));
        test_behind || build_behind
    })
}

fn build_rosen_base_view(snapshot: &RosenBaseSnapshot, verdict: &JobVerdict) -> BaseCi {
    let failing: Vec<String> = verdict
        .active_failures
        .iter()
        .flat_map(|failure| {
            failure
                .jobs
                .iter()
                .map(|job| job_label(&failure.workflow, &job.job))
        })
        .collect();
    let all_green = rosen_all_required_green(snapshot);
    let state = if !failing.is_empty() {
        BaseCiState::Failed
    } else if verdict.running {
        BaseCiState::Running
    } else if all_green && snapshot.last_green.is_some() {
        BaseCiState::Green
    } else {
        BaseCiState::NotRun
    };
    let commit = verdict
        .active_failures
        .first()
        .map(|failure| failure.commit.clone())
        .or_else(|| snapshot.last_green.clone())
        .or_else(|| snapshot.base.clone());
    BaseCi {
        source: BaseCiSource::Rosen,
        state,
        commit,
        failing,
        failures: base_ci_failures(&verdict.active_failures, &verdict.waiting_failures),
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
        if rest.is_empty()
            || rest.starts_with("Process completed with exit code")
            || rest.eq_ignore_ascii_case("The operation was canceled.")
        {
            None
        } else {
            Some(truncate_signature(rest, 180))
        }
    })
}

/// Extracts the test name still running when a timed-out job stopped, from the last
/// `has been running for over 60 seconds` line of the job's stripped log (#584).
pub fn hung_test(log: &str) -> Option<String> {
    log.lines().rev().find_map(parse_hung_test_line)
}

fn parse_hung_test_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let (prefix, _) = trimmed.split_once(HUNG_TEST_MARKER)?;
    let candidate = prefix.trim();
    let name = candidate.strip_prefix("test ").unwrap_or(candidate).trim();
    (!name.is_empty()).then(|| truncate_signature(name, 200))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkflowJobDef {
    id: String,
    name: Option<String>,
    timeout_minutes: TimeoutSetting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimeoutSetting {
    Unspecified,
    Minutes(u64),
    Dynamic,
}

/// Reads the `timeout-minutes` configured for `job_name` in a GitHub Actions workflow YAML
/// file (#584). Matches literal job names, matrix `${{ ... }}` name templates, and job IDs.
/// Returns GitHub's 360-minute default when a matched job omits `timeout-minutes`, and `None`
/// when no job matches or `timeout-minutes` is a dynamic expression.
pub fn workflow_job_timeout_minutes(workflow_yaml: &str, job_name: &str) -> Option<u64> {
    let jobs = parse_workflow_jobs(workflow_yaml);
    let matched = match_workflow_job(&jobs, job_name)?;
    match matched.timeout_minutes {
        TimeoutSetting::Minutes(minutes) => Some(minutes),
        TimeoutSetting::Unspecified => Some(DEFAULT_JOB_TIMEOUT_MINUTES),
        TimeoutSetting::Dynamic => None,
    }
}

fn parse_workflow_jobs(workflow_yaml: &str) -> Vec<WorkflowJobDef> {
    let mut state = WorkflowYamlState::default();
    for raw_line in workflow_yaml.trim_start_matches('\u{feff}').lines() {
        let line = raw_line.trim_end_matches('\r');
        let indent = line.bytes().take_while(|byte| *byte == b' ').count();
        let trimmed = line[indent..].trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !state.step(indent, trimmed) {
            break;
        }
    }
    state.jobs
}

#[derive(Default)]
struct WorkflowYamlState {
    jobs: Vec<WorkflowJobDef>,
    jobs_indent: Option<usize>,
    job_indent: Option<usize>,
    field_indent: Option<usize>,
    block_scalar_indent: Option<usize>,
}

impl WorkflowYamlState {
    fn step(&mut self, indent: usize, trimmed: &str) -> bool {
        if let Some(scalar_indent) = self.block_scalar_indent {
            if indent > scalar_indent {
                return true;
            }
            self.block_scalar_indent = None;
        }
        let Some(jobs_indent) = self.jobs_indent else {
            if let Some((key, value)) = parse_yaml_mapping_line(trimmed) {
                if unquote_yaml_value(key) == "jobs" && strip_unquoted_comment(value).is_empty() {
                    self.jobs_indent = Some(indent);
                } else if is_block_scalar(value) {
                    self.block_scalar_indent = Some(indent);
                }
            }
            return true;
        };
        if indent <= jobs_indent {
            return false;
        }
        let target_job_indent = *self.job_indent.get_or_insert(indent);
        if indent == target_job_indent {
            if let Some((key, value)) = parse_yaml_mapping_line(trimmed) {
                if is_block_scalar(value) {
                    self.block_scalar_indent = Some(indent);
                }
                self.jobs.push(WorkflowJobDef {
                    id: unquote_yaml_value(key),
                    name: None,
                    timeout_minutes: TimeoutSetting::Unspecified,
                });
                self.field_indent = None;
            }
            return true;
        }
        if indent < target_job_indent {
            return true;
        }
        let target_field_indent = *self.field_indent.get_or_insert(indent);
        if let Some((key, value)) = parse_yaml_mapping_line(trimmed) {
            if is_block_scalar(value) {
                self.block_scalar_indent = Some(indent);
            } else if indent == target_field_indent
                && let Some(current) = self.jobs.last_mut()
            {
                apply_job_field(current, &unquote_yaml_value(key), value);
            }
        }
        true
    }
}

fn apply_job_field(job: &mut WorkflowJobDef, key: &str, raw_value: &str) {
    match key {
        "name" => {
            let parsed = unquote_yaml_value(raw_value);
            if !parsed.is_empty() {
                job.name = Some(parsed);
            }
        }
        "timeout-minutes" => {
            let parsed = unquote_yaml_value(raw_value);
            job.timeout_minutes = match parsed.parse::<u64>() {
                Ok(minutes) if minutes > 0 => TimeoutSetting::Minutes(minutes),
                _ => TimeoutSetting::Dynamic,
            };
        }
        _ => {}
    }
}

fn parse_yaml_mapping_line(trimmed: &str) -> Option<(&str, &str)> {
    let bytes = trimmed.as_bytes();
    let mut quote: Option<u8> = None;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(b'"') if byte == b'\\' => index += 1,
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'\'' || byte == b'"' => quote = Some(byte),
            None if byte == b':' => {
                let next = bytes.get(index + 1);
                if next.is_none() || matches!(next, Some(b' ' | b'\t')) {
                    let key = trimmed[..index].trim();
                    let value = trimmed[index + 1..].trim();
                    return (!key.is_empty()).then_some((key, value));
                }
            }
            None => {}
        }
        index += 1;
    }
    None
}

fn strip_unquoted_comment(raw: &str) -> &str {
    let bytes = raw.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'#' && (index == 0 || matches!(bytes[index - 1], b' ' | b'\t')) {
            return raw[..index].trim();
        }
    }
    raw.trim()
}

fn is_block_scalar(raw_value: &str) -> bool {
    let stripped = strip_unquoted_comment(raw_value);
    let mut chars = stripped.chars();
    matches!(chars.next(), Some('|' | '>')) && chars.all(|ch| matches!(ch, '+' | '-' | '0'..='9'))
}

fn unquote_yaml_value(raw_value: &str) -> String {
    let trimmed = raw_value.trim();
    if let Some(rest) = trimmed.strip_prefix('\'') {
        let mut output = String::new();
        let mut chars = rest.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    output.push('\'');
                } else {
                    break;
                }
            } else {
                output.push(ch);
            }
        }
        return output;
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        let mut output = String::new();
        let mut chars = rest.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                if let Some(escaped) = chars.next() {
                    output.push(escaped);
                }
            } else if ch == '"' {
                break;
            } else {
                output.push(ch);
            }
        }
        return output;
    }
    strip_unquoted_comment(trimmed).to_owned()
}

fn match_workflow_job<'a>(
    jobs: &'a [WorkflowJobDef],
    job_name: &str,
) -> Option<&'a WorkflowJobDef> {
    if let Some(exact) = jobs.iter().find(|job| match job.name.as_deref() {
        Some(name) => !name.contains("${{") && name == job_name,
        None => job.id == job_name,
    }) {
        return Some(exact);
    }
    if let Some(templated) = jobs.iter().find(|job| {
        job.name
            .as_deref()
            .is_some_and(|name| template_matches(name, job_name) == Some(true))
    }) {
        return Some(templated);
    }
    if let Some(matrix_id) = jobs
        .iter()
        .find(|job| job.name.is_none() && matrix_id_matches(&job.id, job_name))
    {
        return Some(matrix_id);
    }
    if let Some(by_id) = jobs
        .iter()
        .find(|job| job.id == job_name || matrix_id_matches(&job.id, job_name))
    {
        return Some(by_id);
    }
    let wildcards: Vec<&WorkflowJobDef> = jobs
        .iter()
        .filter(|job| {
            job.name
                .as_deref()
                .is_some_and(|name| template_matches(name, job_name).is_none())
        })
        .collect();
    match wildcards.as_slice() {
        [only] => Some(*only),
        [first, rest @ ..]
            if rest
                .iter()
                .all(|other| other.timeout_minutes == first.timeout_minutes) =>
        {
            Some(*first)
        }
        _ => None,
    }
}

fn matrix_id_matches(job_id: &str, job_name: &str) -> bool {
    job_name
        .strip_prefix(job_id)
        .is_some_and(|rest| rest.starts_with(" (") && rest.ends_with(')'))
}

fn template_matches(template: &str, value: &str) -> Option<bool> {
    let mut literals = Vec::new();
    let mut rest = template;
    while let Some((before, after_open)) = rest.split_once("${{") {
        let Some((_, after_close)) = after_open.split_once("}}") else {
            return Some(template == value);
        };
        literals.push(before);
        rest = after_close;
    }
    literals.push(rest);
    if literals.len() == 1 {
        return Some(template == value);
    }
    if literals.iter().all(|part| part.is_empty()) {
        return None;
    }
    let first = literals[0];
    let last = literals[literals.len() - 1];
    let Some(mut middle) = value.strip_prefix(first) else {
        return Some(false);
    };
    let Some(trimmed) = middle.strip_suffix(last) else {
        return Some(false);
    };
    middle = trimmed;
    for part in &literals[1..literals.len() - 1] {
        if part.is_empty() {
            continue;
        }
        let Some((_, remaining)) = middle.split_once(part) else {
            return Some(false);
        };
        middle = remaining;
    }
    Some(true)
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
            workflow_key: workflow.into(),
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
            ..FailedJob::default()
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
            workflow_key: "Build and lint".into(),
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

    // Risk (#584): GitHub reports timed-out jobs and runs as `cancelled`. The base CI
    // watcher must classify timed-out cancelled jobs as failed (via check-run annotations,
    // or via workflow `timeout-minutes` when annotations cannot be read and no newer run
    // exists), keep push-cancelled runs superseded, and extract the last hung test from logs.
    #[test]
    fn cancelled_timeout_detection_fallback_and_hung_test_extraction() {
        // Primary path: timeout annotation decides even if another run started later.
        let timeout_notes = ["The job has exceeded the maximum execution time of 30m0s"];
        assert!(cancelled_job_timed_out(&CancelledJobEvidence {
            annotations: Some(&timeout_notes),
            has_newer_workflow_run: true,
            elapsed_seconds: Some(1_800),
            timeout_minutes: Some(30),
        }));

        // Push-cancelled run with readable annotations (no timeout annotation) stays superseded.
        let cancel_notes = ["The operation was canceled."];
        assert!(!cancelled_job_timed_out(&CancelledJobEvidence {
            annotations: Some(&cancel_notes),
            has_newer_workflow_run: false,
            elapsed_seconds: Some(1_850),
            timeout_minutes: Some(30),
        }));

        // Fallback when annotations cannot be read: requires no newer run AND elapsed >= timeout.
        assert!(cancelled_job_timed_out(&CancelledJobEvidence {
            annotations: None,
            has_newer_workflow_run: false,
            elapsed_seconds: Some(1_800),
            timeout_minutes: Some(30),
        }));
        assert!(!cancelled_job_timed_out(&CancelledJobEvidence {
            annotations: None,
            has_newer_workflow_run: true,
            elapsed_seconds: Some(1_800),
            timeout_minutes: Some(30),
        }));
        assert!(!cancelled_job_timed_out(&CancelledJobEvidence {
            annotations: None,
            has_newer_workflow_run: false,
            elapsed_seconds: Some(1_799),
            timeout_minutes: Some(30),
        }));

        // Candidate selection picks cancelled runs newer than the last decided run, newest first.
        let candidates = cancelled_candidates(&[
            RunCandidateInput {
                workflow: "CI",
                run: 10,
                state: RunState::Passed,
                cancelled: false,
            },
            RunCandidateInput {
                workflow: "CI",
                run: 11,
                state: RunState::Undecided,
                cancelled: true,
            },
            RunCandidateInput {
                workflow: "CI",
                run: 12,
                state: RunState::Running,
                cancelled: false,
            },
            RunCandidateInput {
                workflow: "Mac",
                run: 20,
                state: RunState::Failed,
                cancelled: false,
            },
            RunCandidateInput {
                workflow: "Mac",
                run: 21,
                state: RunState::Undecided,
                cancelled: true,
            },
            RunCandidateInput {
                workflow: "Old",
                run: 30,
                state: RunState::Undecided,
                cancelled: true,
            },
            RunCandidateInput {
                workflow: "Old",
                run: 31,
                state: RunState::Passed,
                cancelled: false,
            },
        ]);
        assert_eq!(
            candidates,
            vec![
                CancelledCandidate {
                    workflow: "CI".into(),
                    run: 11,
                    has_newer_workflow_run: true,
                },
                CancelledCandidate {
                    workflow: "Mac".into(),
                    run: 21,
                    has_newer_workflow_run: false,
                },
            ]
        );

        // Workflow YAML timeout-minutes parsing: literal name, matrix template, step timeout ignored.
        let yaml = "\
name: CI
on: [push]
jobs:
  test:
    name: test (${{ matrix.os }})
    runs-on: ${{ matrix.os }}
    timeout-minutes: 30 # macOS hang guard
    steps:
      - name: Run tests
        timeout-minutes: 10
        run: |
          echo \"jobs:\"
          cargo test
  lint:
    runs-on: ubuntu-latest
    steps:
      - run: cargo clippy
  dynamic:
    name: custom
    timeout-minutes: ${{ matrix.timeout }}
    steps:
      - run: echo hi
";
        assert_eq!(
            workflow_job_timeout_minutes(yaml, "test (macos-latest)"),
            Some(30)
        );
        assert_eq!(
            workflow_job_timeout_minutes(yaml, "lint"),
            Some(DEFAULT_JOB_TIMEOUT_MINUTES)
        );
        assert_eq!(workflow_job_timeout_minutes(yaml, "custom"), None);
        assert_eq!(workflow_job_timeout_minutes(yaml, "other"), None);

        // Last "has been running for over 60 seconds" line names the hung test.
        let log = "\
test slow_setup has been running for over 60 seconds
test slow_setup ... ok
test collapse_link::collapse_link_and_unlink_round_trip_with_live_server has been running for over 60 seconds
##[error]The operation was canceled.";
        assert_eq!(
            hung_test(log).as_deref(),
            Some("collapse_link::collapse_link_and_unlink_round_trip_with_live_server")
        );
        assert_eq!(
            error_signature("##[error]The operation was canceled."),
            None
        );
    }

    // Risk (#587): when commit A's job fails or times out and a newer, still-running run on
    // commit B has already passed the same job, the watcher must NOT reopen or start a
    // repair for A and must close any open repair for A as resolved by B; when B's matching
    // job is still queued or running, it must wait for B instead of starting a repair.
    #[test]
    fn per_job_evaluation_resolves_passed_jobs_and_waits_for_running_jobs_on_newer_runs() {
        let macos = "macOS (build, clippy, test)";
        let windows = "Windows (build, clippy, test)";
        let linux = "Linux (build, clippy, test)";
        let run_a = BaseRun {
            commit: "a4d9b88000000000000000000000000000000000".into(),
            ..run("CI", 100, RunState::Failed)
        };
        let run_b = BaseRun {
            commit: "33426bb000000000000000000000000000000000".into(),
            ..run("CI", 101, RunState::Running)
        };
        let open_macos = OpenRepair {
            workflow_key: "CI".into(),
            workflow: "CI".into(),
            job: macos.into(),
            commit: "6351d7bd81110000000000000000000000000000".into(),
            run: 90,
            thread: 205,
            landed_commit: Some("520dcfcfed320000000000000000000000000000".into()),
            signature: Some("timed out: collapse_link".into()),
            thread_state: RepairThreadState::Landed,
            commit_order: CommitOrder::AtOrAfterLand,
        };
        let timed_out_macos = FailedJob {
            timed_out: true,
            ..failed_job(macos)
        };

        // 1. Exact #587 sequence: macOS timed out on A, passed on still-running B while
        // Windows is still in progress. No repair starts, open repair for A closes as
        // resolved by B, and the base CI view transitions from Failed to Running.
        let mut jobs = BTreeMap::new();
        jobs.insert(
            100,
            vec![
                RunJob::Failed(timed_out_macos.clone()),
                RunJob::Passed(linux.into()),
                RunJob::Passed(windows.into()),
            ],
        );
        jobs.insert(
            101,
            vec![
                RunJob::Passed(macos.into()),
                RunJob::Passed(linux.into()),
                RunJob::Running(windows.into()),
            ],
        );
        let resolved_verdict = evaluate_jobs(&[run_a.clone(), run_b.clone()], &jobs);
        assert!(resolved_verdict.active_failures.is_empty());
        assert!(resolved_verdict.waiting_failures.is_empty());
        assert!(resolved_verdict.running);
        let resolved_plan = plan_for_verdict(std::slice::from_ref(&open_macos), &resolved_verdict);
        assert!(resolved_plan.start.is_empty());
        assert!(resolved_plan.superseded.is_empty());
        assert_eq!(
            resolved_plan.resolved,
            vec![ResolvedJob {
                workflow_key: "CI".into(),
                workflow: "CI".into(),
                job: macos.into(),
                commit: run_b.commit.clone(),
                run: 101,
            }]
        );
        let resolved_view = view_for_verdict(&resolved_verdict);
        assert_eq!(resolved_view.state, BaseCiState::Running);
        assert!(resolved_view.failing.is_empty());
        assert!(resolved_view.running);

        // 2. While B's matching macOS job is still queued or running, wait for it: neither
        // start a new repair nor reopen the landed repair, while keeping base view Failed.
        jobs.insert(
            101,
            vec![
                RunJob::Running(macos.into()),
                RunJob::Passed(linux.into()),
                RunJob::Running(windows.into()),
            ],
        );
        let waiting_verdict = evaluate_jobs(&[run_a.clone(), run_b.clone()], &jobs);
        assert!(waiting_verdict.active_failures.is_empty());
        assert_eq!(waiting_verdict.waiting_failures.len(), 1);
        let waiting_plan_no_open = plan_for_verdict(&[], &waiting_verdict);
        assert!(waiting_plan_no_open.start.is_empty());
        let waiting_plan_with_open =
            plan_for_verdict(std::slice::from_ref(&open_macos), &waiting_verdict);
        assert!(waiting_plan_with_open.start.is_empty());
        assert!(waiting_plan_with_open.superseded.is_empty());
        assert!(waiting_plan_with_open.resolved.is_empty());
        let waiting_view = view_for_verdict(&waiting_verdict);
        assert_eq!(waiting_view.state, BaseCiState::Failed);
        assert_eq!(waiting_view.commit.as_deref(), Some(run_a.commit.as_str()));
        assert_eq!(waiting_view.failing, [format!("CI · {macos}")]);
        assert!(waiting_view.running);

        // 3. Same when B is freshly queued with no jobs created yet.
        jobs.insert(101, Vec::new());
        let queued_verdict = evaluate_jobs(&[run_a.clone(), run_b.clone()], &jobs);
        assert!(queued_verdict.active_failures.is_empty());
        assert_eq!(queued_verdict.waiting_failures.len(), 1);
        assert!(plan_for_verdict(&[], &queued_verdict).start.is_empty());

        // 4. Mixed in-progress run: macOS passed (closes its repair), Linux failed (starts
        // repair immediately), Windows still running.
        jobs.insert(
            101,
            vec![
                RunJob::Passed(macos.into()),
                RunJob::Failed(failed_job(linux)),
                RunJob::Running(windows.into()),
            ],
        );
        let mixed_verdict = evaluate_jobs(&[run_a.clone(), run_b.clone()], &jobs);
        let mixed_plan = plan_for_verdict(std::slice::from_ref(&open_macos), &mixed_verdict);
        assert_eq!(mixed_plan.resolved.len(), 1);
        assert_eq!(mixed_plan.resolved[0].job, macos);
        assert!(mixed_plan.superseded.is_empty());
        assert_eq!(mixed_plan.start.len(), 1);
        assert_eq!(mixed_plan.start[0].run, 101);
        assert_eq!(mixed_plan.start[0].jobs, vec![failed_job(linux)]);
        assert_eq!(view_for_verdict(&mixed_verdict).state, BaseCiState::Failed);

        // 5. Push-cancelled run B passed macOS before cancellation, and run C is now running
        // macOS: newest finished outcome for macOS is B (Passed), so open repair closes.
        let run_b_cancelled = BaseRun {
            state: RunState::Undecided,
            ..run_b.clone()
        };
        let run_c = BaseRun {
            commit: "4455667000000000000000000000000000000000".into(),
            ..run("CI", 102, RunState::Running)
        };
        let mut chain_jobs = BTreeMap::new();
        chain_jobs.insert(100, vec![RunJob::Failed(timed_out_macos)]);
        chain_jobs.insert(
            101,
            vec![
                RunJob::Passed(macos.into()),
                RunJob::Undecided(windows.into()),
            ],
        );
        chain_jobs.insert(102, vec![RunJob::Running(macos.into())]);
        let chain_verdict = evaluate_jobs(&[run_a, run_b_cancelled, run_c], &chain_jobs);
        let chain_plan = plan_for_verdict(std::slice::from_ref(&open_macos), &chain_verdict);
        assert!(chain_plan.start.is_empty());
        assert!(chain_plan.superseded.is_empty());
        assert_eq!(chain_plan.resolved.len(), 1);
        assert_eq!(chain_plan.resolved[0].run, 101);

        // 6. runs_to_inspect reads newest failed run + newer running runs, and skips clean
        // green workflows with nothing running.
        let inspect = runs_to_inspect(
            &[
                run("Green", 10, RunState::Passed),
                run("CI", 100, RunState::Failed),
                run("CI", 101, RunState::Running),
                run("CI", 102, RunState::Running),
                run("Fresh", 200, RunState::Running),
            ],
            1,
        );
        assert_eq!(inspect, vec![102, 100, 200]);

        // 7. job_state classification covers running, passed, failed, timeout, undecided.
        assert_eq!(job_state(Some("queued"), None, false), JobState::Running);
        assert_eq!(
            job_state(Some("in_progress"), None, false),
            JobState::Running
        );
        assert_eq!(
            job_state(Some("completed"), Some("success"), false),
            JobState::Passed
        );
        assert_eq!(
            job_state(Some("completed"), Some("failure"), false),
            JobState::Failed
        );
        assert_eq!(
            job_state(Some("completed"), Some("cancelled"), true),
            JobState::Failed
        );
        assert_eq!(
            job_state(Some("completed"), Some("cancelled"), false),
            JobState::Undecided
        );
        assert_eq!(
            job_state(Some("completed"), Some("skipped"), false),
            JobState::Undecided
        );
    }

    // Risk (#591): on a GitHub-free project, a macOS-only Rosen post-land failure must
    // produce a failing base CI verdict naming macOS and start a repair thread, then close
    // the repair once macOS turns green without consulting GitHub Actions.
    #[test]
    fn rosen_per_platform_post_land_failure_starts_and_closes_repair() {
        let culprit = "1111111111111111111111111111111111111111";
        let green = "2222222222222222222222222222222222222222";
        let failing_snapshot = RosenBaseSnapshot {
            base: Some(culprit.into()),
            last_green: None,
            platforms: vec![
                RosenPlatformCheckpoint {
                    platform: "windows".into(),
                    display_name: "Windows".into(),
                    required_build: false,
                    required_test: true,
                    last_built: None,
                    last_green: Some(culprit.into()),
                    checker_connected: true,
                },
                RosenPlatformCheckpoint {
                    platform: "macos".into(),
                    display_name: "macOS".into(),
                    required_build: false,
                    required_test: true,
                    last_built: None,
                    last_green: None,
                    checker_connected: true,
                },
            ],
            failures: vec![RosenPostLandFailure {
                session: 42,
                change: "chg-macos-1".into(),
                commit: culprit.into(),
                kind: "test".into(),
                platform: Some("macos".into()),
                platform_display: Some("macOS".into()),
                output: "test unix_socket::binds_socket ... FAILED\n".into(),
                covered_by_green: false,
                reverted: false,
            }],
            bisecting: false,
        };
        let (verdict, view) = evaluate_rosen_post_land(&failing_snapshot);
        assert_eq!(view.source, BaseCiSource::Rosen);
        assert_eq!(view.state, BaseCiState::Failed);
        assert_eq!(view.failing, vec!["Rosen post-land · macOS · test"]);
        let plan = plan_for_verdict(&[], &verdict);
        assert_eq!(plan.start.len(), 1);
        assert_eq!(plan.start[0].run, 42);
        assert_eq!(plan.start[0].jobs[0].job, "macOS · test");
        assert_eq!(plan.start[0].jobs[0].runner.as_deref(), Some("macOS"));

        let open = [OpenRepair {
            workflow: ROSEN_BASE_WORKFLOW.into(),
            job: "macOS · test".into(),
            commit: culprit.into(),
            run: 42,
            thread: 7,
            ..OpenRepair::default()
        }];
        let green_snapshot = RosenBaseSnapshot {
            base: Some(green.into()),
            last_green: Some(green.into()),
            platforms: vec![
                RosenPlatformCheckpoint {
                    platform: "windows".into(),
                    display_name: "Windows".into(),
                    required_build: false,
                    required_test: true,
                    last_built: None,
                    last_green: Some(green.into()),
                    checker_connected: true,
                },
                RosenPlatformCheckpoint {
                    platform: "macos".into(),
                    display_name: "macOS".into(),
                    required_build: false,
                    required_test: true,
                    last_built: None,
                    last_green: Some(green.into()),
                    checker_connected: true,
                },
            ],
            failures: vec![RosenPostLandFailure {
                covered_by_green: true,
                ..failing_snapshot.failures[0].clone()
            }],
            bisecting: false,
        };
        let (green_verdict, green_view) = evaluate_rosen_post_land(&green_snapshot);
        assert_eq!(green_view.source, BaseCiSource::Rosen);
        assert_eq!(green_view.state, BaseCiState::Green);
        let green_plan = plan_for_verdict(&open, &green_verdict);
        assert!(green_plan.start.is_empty());
        assert_eq!(green_plan.closed, vec![ROSEN_BASE_WORKFLOW]);
    }

    #[test]
    fn collapse_mirror_always_uses_rosen_ci_whatever_the_issue_host() {
        use crate::collapse::IssueHost;

        assert_eq!(
            effective_base_ci_source(true, IssueHost::GitHub, Some(BaseCiSource::GitHub), true),
            BaseCiSource::Rosen
        );
        assert_eq!(
            effective_base_ci_source(true, IssueHost::GitHub, None, true),
            BaseCiSource::Rosen
        );
        assert_eq!(
            effective_base_ci_source(true, IssueHost::Collapse, None, true),
            BaseCiSource::Rosen
        );
        assert_eq!(
            effective_base_ci_source(false, IssueHost::GitHub, None, true),
            BaseCiSource::GitHub
        );
        assert_eq!(
            effective_base_ci_source(false, IssueHost::GitHub, Some(BaseCiSource::Rosen), true),
            BaseCiSource::Rosen
        );
        assert_eq!(
            effective_base_ci_source(false, IssueHost::GitHub, Some(BaseCiSource::GitHub), false),
            BaseCiSource::Rosen
        );
    }

    // Risk (#652): a GitHub App check run with no Actions run or commit status must surface
    // as failing and start one repair, a later pass from the same app with the same name must
    // settle it (while a different app with the same name must not), and an Actions job's own
    // check-run record must never produce a duplicate failure or second repair.
    #[test]
    fn standalone_github_app_check_runs_start_settle_and_skip_actions_duplicates() {
        let commit_a = "a".repeat(40);
        let commit_b = "b".repeat(40);
        let commit_page_a = format!("https://github.com/mESmaC/velocity/commit/{commit_a}");
        let commit_page_b = format!("https://github.com/mESmaC/velocity/commit/{commit_b}");

        // 1. Standalone failed check from an external GitHub App (no Actions runs, no statuses).
        let failed_check = CheckRunInput {
            id: 501,
            name: "macOS tests".into(),
            head_sha: commit_a.clone(),
            status: Some("completed".into()),
            conclusion: Some("failure".into()),
            app_id: Some(24),
            app_slug: Some("external-ci".into()),
            html_url: Some("https://github.com/mESmaC/velocity/runs/501".into()),
            output_title: Some("macOS suite failed".into()),
            output_summary: Some("1 failing test".into()),
            ..CheckRunInput::default()
        };
        let runs_a =
            check_runs_to_base_runs(&[failed_check], &commit_a, &commit_page_a, &BTreeSet::new());
        let verdict_a = evaluate_jobs(&runs_a, &BTreeMap::new());
        let view_a = view_for_verdict(&verdict_a);
        assert_eq!(view_a.state, BaseCiState::Failed);
        assert_eq!(view_a.commit.as_deref(), Some(commit_a.as_str()));
        assert_eq!(view_a.failing, vec!["external-ci · macOS tests"]);
        let first_plan = plan_for_verdict(&[], &verdict_a);
        assert_eq!(first_plan.start.len(), 1);
        assert_eq!(first_plan.start[0].workflow, "external-ci · macOS tests");
        assert_eq!(
            first_plan.start[0].detail,
            "macOS suite failed: 1 failing test"
        );

        // Polling again while the repair is active starts no second repair.
        let open = [OpenRepair {
            workflow: first_plan.start[0].workflow.clone(),
            job: first_plan.start[0].jobs[0].job.clone(),
            commit: commit_a.clone(),
            run: 0,
            thread: 42,
            thread_state: RepairThreadState::Active,
            ..OpenRepair::default()
        }];
        let repeat_plan = plan_for_verdict(&open, &verdict_a);
        assert!(repeat_plan.start.is_empty());
        assert!(repeat_plan.closed.is_empty());

        // 2. A passing check with the same name from a DIFFERENT app does not settle the repair.
        let other_app_pass = CheckRunInput {
            id: 600,
            name: "macOS tests".into(),
            head_sha: commit_b.clone(),
            status: Some("completed".into()),
            conclusion: Some("success".into()),
            app_id: Some(99),
            app_slug: Some("other-ci".into()),
            ..CheckRunInput::default()
        };
        let other_runs = check_runs_to_base_runs(
            &[other_app_pass],
            &commit_b,
            &commit_page_b,
            &BTreeSet::new(),
        );
        let other_verdict = evaluate_jobs(&other_runs, &BTreeMap::new());
        assert!(plan_for_verdict(&open, &other_verdict).closed.is_empty());

        // 3. A later passing check from the SAME app with the SAME name settles the repair.
        let same_app_pass = CheckRunInput {
            id: 601,
            name: "macOS tests".into(),
            head_sha: commit_b.clone(),
            status: Some("completed".into()),
            conclusion: Some("success".into()),
            app_id: Some(24),
            app_slug: Some("external-ci".into()),
            ..CheckRunInput::default()
        };
        let passed_runs = check_runs_to_base_runs(
            &[same_app_pass],
            &commit_b,
            &commit_page_b,
            &BTreeSet::new(),
        );
        let passed_verdict = evaluate_jobs(&passed_runs, &BTreeMap::new());
        let settled_plan = plan_for_verdict(&open, &passed_verdict);
        assert_eq!(settled_plan.closed, vec!["external-ci · macOS tests"]);
        assert!(settled_plan.start.is_empty());
        let passed_view = view_for_verdict(&passed_verdict);
        assert_eq!(passed_view.state, BaseCiState::Green);
        assert_eq!(passed_view.commit.as_deref(), Some(commit_b.as_str()));

        // 4. An Actions job's check-run record never produces a second repair alongside its
        // Actions workflow run, whether identified by app slug, app id, URL or job id.
        let actions_run = BaseRun {
            commit: commit_a.clone(),
            ..run("CI", 77, RunState::Failed)
        };
        let mut actions_jobs = BTreeMap::new();
        actions_jobs.insert(
            77,
            vec![RunJob::Failed(FailedJob {
                id: 9001,
                ..failed_job("macOS tests")
            })],
        );
        let actions_job_ids = BTreeSet::from([9001]);
        let duplicate_checks = [
            CheckRunInput {
                id: 9001,
                name: "macOS tests".into(),
                head_sha: commit_a.clone(),
                status: Some("completed".into()),
                conclusion: Some("failure".into()),
                app_id: Some(GITHUB_ACTIONS_APP_ID),
                app_slug: Some(GITHUB_ACTIONS_APP_SLUG.into()),
                html_url: Some(
                    "https://github.com/mESmaC/velocity/actions/runs/77/job/9001".into(),
                ),
                ..CheckRunInput::default()
            },
            CheckRunInput {
                id: 9002,
                name: "Linux tests".into(),
                head_sha: commit_a.clone(),
                status: Some("completed".into()),
                conclusion: Some("failure".into()),
                app_id: Some(GITHUB_ACTIONS_APP_ID),
                ..CheckRunInput::default()
            },
            CheckRunInput {
                id: 9003,
                name: "Dispatched job".into(),
                head_sha: commit_a.clone(),
                status: Some("completed".into()),
                conclusion: Some("failure".into()),
                html_url: Some(
                    "https://github.com/mESmaC/velocity/actions/runs/88/job/9003".into(),
                ),
                ..CheckRunInput::default()
            },
        ];
        let mut combined_runs = vec![actions_run];
        combined_runs.extend(check_runs_to_base_runs(
            &duplicate_checks,
            &commit_a,
            &commit_page_a,
            &actions_job_ids,
        ));
        let actions_verdict = evaluate_jobs(&combined_runs, &actions_jobs);
        let actions_plan = plan_for_verdict(&[], &actions_verdict);
        assert_eq!(actions_plan.start.len(), 1);
        assert_eq!(actions_plan.start[0].workflow, "CI");
        assert_eq!(actions_plan.start[0].run, 77);
        assert_eq!(actions_plan.start[0].jobs.len(), 1);
        assert_eq!(actions_plan.start[0].jobs[0].job, "macOS tests");
    }

    // Risk (#653): two distinct GitHub Actions workflow files can both be named `CI`. A
    // newer green docs workflow must neither hide a red macOS workflow on the first poll
    // nor close/resolve the macOS repair on a later poll, while both keep display name `CI`.
    #[test]
    fn same_name_workflows_stay_independent_in_verdict_and_repairs() {
        let macos_key = actions_workflow_key(Some(11), Some(".github/workflows/macos.yml"), "CI");
        let docs_key = actions_workflow_key(Some(12), Some(".github/workflows/docs.yml"), "CI");
        assert_eq!(macos_key, "workflow:11");
        assert_eq!(docs_key, "workflow:12");

        let failed_macos = BaseRun {
            workflow_key: macos_key.clone(),
            ..run("CI", 101, RunState::Failed)
        };
        let passed_docs = BaseRun {
            workflow_key: docs_key.clone(),
            ..run("CI", 102, RunState::Passed)
        };

        // 1. Classic verdict and runs_to_inspect keep both workflows independent.
        let combined = [failed_macos.clone(), passed_docs.clone()];
        let summary = verdict(&combined);
        assert_eq!(summary.failed, vec![failed_macos.clone()]);
        assert_eq!(summary.passed, vec![docs_key.clone()]);
        assert_eq!(runs_to_inspect(&combined, 5), vec![101]);

        // 2. Per-job verdict preserves the red macOS failure and display name `CI`.
        let mut jobs = BTreeMap::new();
        jobs.insert(101, vec![RunJob::Failed(failed_job("test"))]);
        jobs.insert(102, vec![RunJob::Passed("test".into())]);
        let job_verdict = evaluate_jobs(&combined, &jobs);
        assert_eq!(job_verdict.active_failures.len(), 1);
        assert_eq!(job_verdict.active_failures[0].workflow_key, macos_key);
        assert_eq!(job_verdict.active_failures[0].workflow, "CI");
        assert_eq!(job_verdict.active_failures[0].run, 101);
        assert_eq!(job_verdict.passed, vec![docs_key.clone()]);
        let ci_view = view_for_verdict(&job_verdict);
        assert_eq!(ci_view.state, BaseCiState::Failed);
        assert_eq!(ci_view.failing, vec!["CI · test"]);

        // 3. First poll starts a repair; second poll with the green docs workflow (even with
        // an identically named `test` job) neither closes nor resolves the macOS repair.
        let first_plan = plan_for_verdict(&[], &job_verdict);
        assert_eq!(first_plan.start.len(), 1);
        assert_eq!(first_plan.start[0].workflow_key, macos_key);
        assert_eq!(first_plan.start[0].workflow, "CI");

        let open_macos = [OpenRepair {
            workflow_key: macos_key.clone(),
            workflow: "CI".into(),
            job: "test".into(),
            commit: failed_macos.commit.clone(),
            run: 101,
            thread: 7,
            thread_state: RepairThreadState::Active,
            ..OpenRepair::default()
        }];
        let second_plan = plan_for_verdict(&open_macos, &job_verdict);
        assert!(second_plan.closed.is_empty());
        assert!(second_plan.resolved.is_empty());
        assert!(second_plan.start.is_empty());

        // 4. Only a green run of workflow 11 closes the macOS repair (even if renamed).
        let passed_macos = BaseRun {
            workflow_key: macos_key.clone(),
            ..run("macOS CI", 103, RunState::Passed)
        };
        let green_verdict = evaluate_jobs(&[failed_macos, passed_docs, passed_macos], &jobs);
        let green_plan = plan_for_verdict(&open_macos, &green_verdict);
        assert_eq!(green_plan.closed, vec![macos_key.clone()]);
        assert_eq!(view_for_verdict(&green_verdict).state, BaseCiState::Green);

        // 5. Cancelled timeout candidate selection also keeps workflows separate.
        let candidates = cancelled_candidates(&[
            RunCandidateInput {
                workflow: &macos_key,
                run: 101,
                state: RunState::Undecided,
                cancelled: true,
            },
            RunCandidateInput {
                workflow: &docs_key,
                run: 102,
                state: RunState::Passed,
                cancelled: false,
            },
        ]);
        assert_eq!(
            candidates,
            vec![CancelledCandidate {
                workflow: macos_key,
                run: 101,
                has_newer_workflow_run: false,
            }]
        );
    }

    // Risk (#697): project health must pair a selected repair with its own workflow/job
    // failing commit rather than project-wide `BaseCi::commit`, so another workflow's newer
    // red commit never causes health to claim a queued replacement while the poller waits.
    #[test]
    fn health_pairs_repair_with_its_own_workflow_and_job_failure() {
        let c1 = "1111111111111111111111111111111111111111";
        let c2 = "2222222222222222222222222222222222222222";
        let c3 = "3333333333333333333333333333333333333333";
        let c4 = "4444444444444444444444444444444444444444";

        // 1. Exact two-workflow #697 fixture: Build landed at c3 but its latest failed run is
        // still c2; Docs failed later at c4 with its same-error repair already terminal.
        let build_run = BaseRun {
            workflow_key: "workflow:1".into(),
            commit: c2.into(),
            ..run("Build", 10, RunState::Failed)
        };
        let docs_run = BaseRun {
            workflow_key: "workflow:2".into(),
            commit: c4.into(),
            ..run("Docs", 20, RunState::Failed)
        };
        let mut jobs = BTreeMap::new();
        jobs.insert(10, vec![RunJob::Failed(failed_job("test"))]);
        jobs.insert(20, vec![RunJob::Failed(failed_job("lint"))]);
        let verdict = evaluate_jobs(&[build_run.clone(), docs_run.clone()], &jobs);
        let view = view_for_verdict(&verdict);
        assert_eq!(view.commit.as_deref(), Some(c4));
        assert_eq!(view.failures.len(), 2);

        let candidates = [
            RepairCandidate {
                workflow_key: "workflow:2",
                workflow: "Docs",
                job: "lint",
                active: false,
            },
            RepairCandidate {
                workflow_key: "workflow:1",
                workflow: "Build",
                job: "test",
                active: false,
            },
        ];
        assert_eq!(select_ci_repair_index(&candidates, &view), Some(1));
        let matched = matching_base_ci_failure(&view, "workflow:1", "Build", "test");
        assert_eq!(
            matched,
            Some(MatchedBaseCiFailure {
                commit: c2,
                waiting: false,
            })
        );
        let failing_commit = repair_health_failing_commit(RepairThreadState::Landed, matched);
        assert_eq!(failing_commit, Some(c2));

        let build_repair = OpenRepair {
            workflow_key: "workflow:1".into(),
            workflow: "Build".into(),
            job: "test".into(),
            commit: c2.into(),
            run: 10,
            thread: 12,
            landed_commit: Some(c3.into()),
            thread_state: RepairThreadState::Landed,
            commit_order: CommitOrder::BeforeLand,
            ..OpenRepair::default()
        };
        let docs_repair = OpenRepair {
            workflow_key: "workflow:2".into(),
            workflow: "Docs".into(),
            job: "lint".into(),
            commit: c1.into(),
            run: 5,
            thread: 11,
            thread_state: RepairThreadState::Error,
            ..OpenRepair::default()
        };
        let poller_plan = plan_for_verdict(&[docs_repair, build_repair.clone()], &verdict);
        assert!(poller_plan.superseded.is_empty());
        assert!(poller_plan.start.is_empty());

        let action = format_ci_repair_action(Some(&build_repair), failing_commit, true);
        assert!(
            action.contains("waiting for CI on 333333333333"),
            "{action}"
        );
        assert!(!action.contains("new fix-forward queued"), "{action}");

        // 2. Once Build itself fails at c4 (after c3), health matches c4 and reports the
        // queued replacement, matching the poller.
        let build_after_land = BaseRun {
            workflow_key: "workflow:1".into(),
            commit: c4.into(),
            ..run("Build", 30, RunState::Failed)
        };
        jobs.insert(30, vec![RunJob::Failed(failed_job("test"))]);
        let after_verdict = evaluate_jobs(&[build_run, docs_run, build_after_land.clone()], &jobs);
        let after_view = view_for_verdict(&after_verdict);
        let after_matched = matching_base_ci_failure(&after_view, "workflow:1", "Build", "test");
        assert_eq!(
            after_matched,
            Some(MatchedBaseCiFailure {
                commit: c4,
                waiting: false,
            })
        );
        let after_repair = OpenRepair {
            commit_order: CommitOrder::AtOrAfterLand,
            ..build_repair.clone()
        };
        let after_plan = plan_for_verdict(std::slice::from_ref(&after_repair), &after_verdict);
        assert_eq!(after_plan.superseded.len(), 1);
        let after_action = format_ci_repair_action(
            Some(&after_repair),
            repair_health_failing_commit(RepairThreadState::Landed, after_matched),
            true,
        );
        assert!(
            after_action.contains("latest failure 444444444444 is newer than its land"),
            "{after_action}"
        );
        assert!(
            after_action.contains("new fix-forward queued"),
            "{after_action}"
        );

        // 3. When a newer Build run is already running `test`, the poller waits and health
        // must also report waiting for CI rather than queuing a replacement.
        let build_running = BaseRun {
            workflow_key: "workflow:1".into(),
            commit: c4.into(),
            ..run("Build", 31, RunState::Running)
        };
        jobs.insert(31, vec![RunJob::Running("test".into())]);
        let waiting_verdict = evaluate_jobs(&[build_after_land, build_running], &jobs);
        let waiting_view = view_for_verdict(&waiting_verdict);
        let waiting_matched =
            matching_base_ci_failure(&waiting_view, "workflow:1", "Build", "test");
        assert_eq!(
            waiting_matched,
            Some(MatchedBaseCiFailure {
                commit: c4,
                waiting: true,
            })
        );
        assert!(
            plan_for_verdict(std::slice::from_ref(&after_repair), &waiting_verdict)
                .superseded
                .is_empty()
        );
        let waiting_commit =
            repair_health_failing_commit(RepairThreadState::Landed, waiting_matched);
        assert_eq!(waiting_commit, None);
        let waiting_repair = OpenRepair {
            commit_order: CommitOrder::Unknown,
            ..build_repair
        };
        let waiting_action = format_ci_repair_action(Some(&waiting_repair), waiting_commit, true);
        assert!(
            waiting_action.contains("waiting for CI"),
            "{waiting_action}"
        );
        assert!(
            !waiting_action.contains("new fix-forward queued"),
            "{waiting_action}"
        );

        // 4. Legacy BaseCi without `failures` never guesses across multiple failing jobs, and
        // only falls back to `ci.commit` when the single failing entry matches the repair.
        let legacy_multi = BaseCi {
            source: BaseCiSource::GitHub,
            state: BaseCiState::Failed,
            commit: Some(c4.into()),
            failing: vec!["Build · test".into(), "Docs · lint".into()],
            failures: Vec::new(),
            running: false,
        };
        assert_eq!(
            matching_base_ci_failure(&legacy_multi, "workflow:1", "Build", "test"),
            None
        );
        let legacy_single = BaseCi {
            failing: vec!["Build · test".into()],
            ..legacy_multi
        };
        assert_eq!(
            matching_base_ci_failure(&legacy_single, "workflow:1", "Build", "test"),
            Some(MatchedBaseCiFailure {
                commit: c4,
                waiting: false,
            })
        );
    }
}
