//! GitHub's side of the base's CI (SPEC.md F5 "Rosen · Base CI", #507, #652): the workflow runs
//! on a Rosen project's mirrored base branch, the commit statuses and standalone GitHub App check
//! runs on its tip, and a failed run's failed jobs and their log. `velocity_core::base_ci`
//! decides what they mean. Everything is REST through `gh`, which keeps working when a swarm has
//! spent the GraphQL budget.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

use serde::Deserialize;
use velocity_core::base_ci::{
    self, BaseRun, CancelledJobEvidence, CheckRunInput, FailedJob, Failure, JobPlatformOutcome,
    JobState, RunCandidateInput, RunJob, RunState,
};

use crate::GitHubCommandError;
use crate::workflow_runs::{Annotation, WorkflowClient, WorkflowJob, WorkflowRun};

/// How many of the branch's newest runs one look reads. This covers several workflows with
/// runs queued or running behind them.
const RUNS_READ: usize = 50;
/// Cap on cancelled runs inspected per workflow in one look so a burst of push-cancels
/// cannot flood GitHub with job/annotation reads (#584).
const MAX_CANCELLED_CHECKS_PER_WORKFLOW: usize = 5;
/// Cap on in-progress runs inspected per workflow in one look so a backlog of queued runs
/// cannot flood GitHub with job reads (#587).
const MAX_RUNNING_CHECKS_PER_WORKFLOW: usize = 5;
/// Cap on cancelled jobs whose check-run annotations are read in one run (#584).
const MAXIMUM_ANNOTATED_JOBS: usize = 30;
/// How many check runs each page of the tip commit's check-runs endpoint reads (#652).
const CHECK_RUNS_PER_PAGE: usize = 100;
/// Cap on check-run pages read for the base tip commit in one look (#652).
const MAX_CHECK_RUN_PAGES: usize = 5;

/// The `gh` transport for the repository a Rosen project's base is mirrored to.
pub struct BaseCiWatch {
    client: WorkflowClient,
}

/// Runs on the base branch together with inspected per-run jobs, avoiding redundant GitHub
/// requests in the same look (#584, #587, #685).
#[derive(Clone, Debug, Default)]
pub struct BaseRunSnapshot {
    pub runs: Vec<BaseRun>,
    pub run_jobs: BTreeMap<u64, Vec<RunJob>>,
    /// Per-job states and runner labels for inspected runs (used by deploy platform gating, #685).
    pub run_job_outcomes: BTreeMap<u64, Vec<JobPlatformOutcome>>,
}

/// Cache shared across consecutive [`BaseCiWatch::runs`] polls for one project so already-completed
/// Actions runs and settled tip check runs are not re-polled on every tick (#671).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BaseCiPollCache {
    repository: String,
    branch: String,
    completed_run_jobs: BTreeMap<u64, CachedRunJobs>,
    tip_check_runs: CachedTipCheckRuns,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RunJobsStamp {
    run_attempt: Option<u64>,
    conclusion: Option<String>,
    updated_at: Option<String>,
}

impl RunJobsStamp {
    fn from_workflow_run(run: &WorkflowRun) -> Self {
        Self {
            run_attempt: run.run_attempt,
            conclusion: run.conclusion.clone(),
            updated_at: run.updated_at.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CachedRunJobs {
    stamp: RunJobsStamp,
    jobs: Vec<RunJob>,
    outcomes: Vec<JobPlatformOutcome>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum CachedTipCheckRuns {
    #[default]
    None,
    /// Observed an empty check-runs list once for `sha`; re-check once on the next poll
    /// before treating it as settled so late-starting check runs right after a push are not missed.
    EmptyOnce { sha: String },
    /// All check runs for `sha` are completed (or remained empty across consecutive polls).
    Settled {
        sha: String,
        checks: Vec<CheckRunInput>,
    },
}

impl BaseCiPollCache {
    /// Clears all cached run jobs and tip check-runs state.
    pub fn clear(&mut self) {
        self.repository.clear();
        self.branch.clear();
        self.completed_run_jobs.clear();
        self.tip_check_runs = CachedTipCheckRuns::None;
    }

    fn reset_if_target_changed(&mut self, repository: &str, branch: &str) {
        if self.repository != repository || self.branch != branch {
            self.repository = repository.to_owned();
            self.branch = branch.to_owned();
            self.completed_run_jobs.clear();
            self.tip_check_runs = CachedTipCheckRuns::None;
        }
    }

    fn prune_completed_runs(&mut self, completed_stamps: &BTreeMap<u64, RunJobsStamp>) {
        self.completed_run_jobs.retain(|run_id, cached| {
            completed_stamps
                .get(run_id)
                .is_some_and(|stamp| *stamp == cached.stamp)
        });
    }

    fn completed_jobs(
        &self,
        run_id: u64,
        stamp: Option<&RunJobsStamp>,
    ) -> Option<InspectedRunJobs> {
        let stamp = stamp?;
        let cached = self.completed_run_jobs.get(&run_id)?;
        (cached.stamp == *stamp).then(|| InspectedRunJobs {
            jobs: cached.jobs.clone(),
            outcomes: cached.outcomes.clone(),
        })
    }

    fn record_completed_jobs(
        &mut self,
        run_id: u64,
        stamp: Option<&RunJobsStamp>,
        inspected: &InspectedRunJobs,
    ) {
        let Some(stamp) = stamp else {
            return;
        };
        // Avoid caching if GitHub's jobs endpoint still reports a running job due to eventual
        // consistency right as a workflow run transitions to `completed`.
        if inspected
            .jobs
            .iter()
            .all(|job| job.state() != JobState::Running)
        {
            self.completed_run_jobs.insert(
                run_id,
                CachedRunJobs {
                    stamp: stamp.clone(),
                    jobs: inspected.jobs.clone(),
                    outcomes: inspected.outcomes.clone(),
                },
            );
        }
    }

    fn settled_tip_checks(
        &self,
        tip_sha: &str,
        has_running_actions_on_tip: bool,
    ) -> Option<&[CheckRunInput]> {
        let tip_sha = tip_sha.trim();
        if tip_sha.is_empty() || has_running_actions_on_tip {
            return None;
        }
        match &self.tip_check_runs {
            CachedTipCheckRuns::Settled { sha, checks } if sha == tip_sha => {
                Some(checks.as_slice())
            }
            _ => None,
        }
    }

    fn record_tip_checks(
        &mut self,
        tip_sha: &str,
        has_running_actions_on_tip: bool,
        truncated: bool,
        checks: &[CheckRunInput],
    ) {
        let tip_sha = tip_sha.trim();
        if tip_sha.is_empty() || has_running_actions_on_tip || truncated {
            self.tip_check_runs = CachedTipCheckRuns::None;
            return;
        }
        if !checks.is_empty() && checks.iter().all(is_check_run_completed) {
            self.tip_check_runs = CachedTipCheckRuns::Settled {
                sha: tip_sha.to_owned(),
                checks: checks.to_vec(),
            };
        } else if checks.is_empty() {
            let confirmed_empty = matches!(
                &self.tip_check_runs,
                CachedTipCheckRuns::EmptyOnce { sha } if sha == tip_sha
            );
            self.tip_check_runs = if confirmed_empty {
                CachedTipCheckRuns::Settled {
                    sha: tip_sha.to_owned(),
                    checks: Vec::new(),
                }
            } else {
                CachedTipCheckRuns::EmptyOnce {
                    sha: tip_sha.to_owned(),
                }
            };
        } else {
            self.tip_check_runs = CachedTipCheckRuns::None;
        }
    }
}

fn is_check_run_completed(check: &CheckRunInput) -> bool {
    check.status.as_deref() == Some("completed")
}

/// Per-run jobs together with runner labels returned by one GitHub jobs inspection (#685).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct InspectedRunJobs {
    jobs: Vec<RunJob>,
    outcomes: Vec<JobPlatformOutcome>,
}

/// A failed run's failed steps' output, and where the whole of it was saved.
#[derive(Clone, Debug, Default)]
pub struct FailedLog {
    pub text: String,
    pub saved: Option<PathBuf>,
}

impl BaseCiWatch {
    /// `repository` is the mirror's `owner/name`. No request is made here.
    #[track_caller]
    pub fn new(repository: &str) -> Result<Self, GitHubCommandError> {
        Ok(Self {
            client: WorkflowClient::for_mirror(repository)?,
        })
    }

    /// The runs GitHub started for commits of `branch`, and the statuses on its tip.
    /// Cancelled runs newer than each workflow's latest decided run are inspected so a
    /// timed-out job (reported as `cancelled` by GitHub) counts as failed (#584).
    /// Completed run jobs and settled tip check runs are reused from `cache` across polls (#671).
    pub fn runs(
        &self,
        branch: &str,
        cache: &mut BaseCiPollCache,
    ) -> Result<BaseRunSnapshot, GitHubCommandError> {
        self.runs_with_platform_jobs(branch, cache, false, &BTreeSet::new())
    }

    /// Like [`Self::runs`], optionally also inspecting each workflow's newest passing run
    /// and any run for `watched_commits` so deploy platform gating sees runner labels (#685).
    pub fn runs_with_platform_jobs(
        &self,
        branch: &str,
        cache: &mut BaseCiPollCache,
        include_newest_passed: bool,
        watched_commits: &BTreeSet<String>,
    ) -> Result<BaseRunSnapshot, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Runs {
            workflow_runs: Vec<WorkflowRun>,
        }
        let path = format!(
            "/actions/runs?branch={}&per_page={RUNS_READ}&exclude_pull_requests=true",
            encoded(branch, false)
        );
        let raw_runs: Vec<WorkflowRun> = self
            .client
            .read::<Runs>(&path)?
            .workflow_runs
            .into_iter()
            .filter(|run| run.event.as_deref().is_some_and(base_ci::counts_for_base))
            .collect();
        cache.reset_if_target_changed(self.client.repository(), branch);
        let PreparedRuns {
            mut runs,
            workflow_paths,
            cancelled_ids,
            completed_stamps,
        } = prepare_workflow_runs(raw_runs);
        cache.prune_completed_runs(&completed_stamps);
        let mut inspect_jobs = |run, commit: &str, workflow_path: Option<&str>, has_newer| {
            self.inspect_run_jobs(run, commit, workflow_path, has_newer)
        };
        let (mut run_jobs, mut run_job_outcomes) = promote_timed_out_runs(
            &mut runs,
            &cancelled_ids,
            &workflow_paths,
            &completed_stamps,
            cache,
            &mut inspect_jobs,
        )?;
        inspect_relevant_runs(
            &runs,
            &workflow_paths,
            &completed_stamps,
            include_newest_passed,
            watched_commits,
            &mut run_jobs,
            &mut run_job_outcomes,
            cache,
            &mut inspect_jobs,
        )?;
        let (tip_sha, status_runs) = self.statuses(branch)?;
        let has_running_actions_on_tip = has_running_actions_for_tip(&runs, &tip_sha);
        runs.extend(status_runs);
        let actions_job_ids = failed_actions_job_ids(&run_jobs);
        let check_runs = resolve_check_runs(
            self.client.repository(),
            &tip_sha,
            has_running_actions_on_tip,
            &actions_job_ids,
            cache,
            || self.fetch_check_runs(branch, &tip_sha),
        )?;
        runs.extend(check_runs);
        Ok(BaseRunSnapshot {
            runs,
            run_jobs,
            run_job_outcomes,
        })
    }

    /// The tip's commit statuses, which is how legacy CI other than GitHub Actions reports.
    /// GitHub's combined status holds each context's newest status and the tip commit SHA.
    fn statuses(&self, branch: &str) -> Result<(String, Vec<BaseRun>), GitHubCommandError> {
        #[derive(Deserialize)]
        struct Combined {
            sha: String,
            #[serde(default)]
            statuses: Vec<Status>,
        }
        #[derive(Deserialize)]
        struct Status {
            context: String,
            state: String,
            target_url: Option<String>,
            description: Option<String>,
        }
        let path = format!(
            "/commits/heads/{}/status?per_page=100",
            encoded(branch, true)
        );
        let combined: Combined = self.client.read(&path)?;
        let commit_page = format!(
            "https://github.com/{}/commit/{}",
            self.client.repository(),
            combined.sha
        );
        let runs = combined
            .statuses
            .into_iter()
            .filter(|status| status.context != base_ci::INTEGRATION_CONTEXT)
            .map(|status| BaseRun {
                workflow_key: status.context.clone(),
                workflow: status.context,
                run: 0,
                commit: combined.sha.clone(),
                url: status.target_url.unwrap_or_else(|| commit_page.clone()),
                state: base_ci::status_state(&status.state),
                detail: status.description.unwrap_or_default(),
            })
            .collect();
        Ok((combined.sha, runs))
    }

    /// Reads raw check runs on the base branch's tip commit (#652), reporting whether
    /// pagination stopped at `MAX_CHECK_RUN_PAGES` before exhausting all check runs.
    fn fetch_check_runs(
        &self,
        branch: &str,
        tip_sha: &str,
    ) -> Result<(Vec<CheckRunInput>, bool), GitHubCommandError> {
        let reference = if tip_sha.trim().is_empty() {
            format!("heads/{}", encoded(branch, true))
        } else {
            encoded(tip_sha.trim(), false)
        };
        let base_path = format!("/commits/{reference}/check-runs?per_page={CHECK_RUNS_PER_PAGE}");
        let mut checks = Vec::new();
        let mut truncated = true;
        for page in 1..=MAX_CHECK_RUN_PAGES {
            let path = if page == 1 {
                base_path.clone()
            } else {
                format!("{base_path}&page={page}")
            };
            let response: CheckRunsResponse = self.client.read(&path)?;
            let received = response.check_runs.len();
            checks.extend(response.check_runs.into_iter().map(ApiCheckRun::into_input));
            if received < CHECK_RUNS_PER_PAGE
                || (response.total_count > 0 && checks.len() >= response.total_count)
            {
                truncated = false;
                break;
            }
        }
        Ok((checks, truncated))
    }

    fn inspect_run_jobs(
        &self,
        run: u64,
        commit: &str,
        workflow_path: Option<&str>,
        has_newer_workflow_run: bool,
    ) -> Result<InspectedRunJobs, GitHubCommandError> {
        let jobs = self.client.jobs(run)?;
        let timed_out_cancelled =
            self.timed_out_cancelled_indices(&jobs, commit, workflow_path, has_newer_workflow_run);
        let mut inspected = Vec::with_capacity(jobs.len());
        let mut outcomes = Vec::with_capacity(jobs.len());
        for (index, job) in jobs.into_iter().enumerate() {
            let timed_out_cancelled = timed_out_cancelled.contains(&index);
            let state = base_ci::job_state(
                job.status.as_deref(),
                job.conclusion.as_deref(),
                timed_out_cancelled,
            );
            outcomes.push(JobPlatformOutcome {
                job: job.name.clone(),
                state,
                labels: job.labels.clone(),
            });
            let entry = match state {
                JobState::Passed => RunJob::Passed(job.name),
                JobState::Failed => {
                    let timed_out = timed_out_cancelled
                        || base_ci::job_timed_out_directly(job.conclusion.as_deref());
                    RunJob::Failed(failed_job(job, timed_out))
                }
                JobState::Running => RunJob::Running(job.name),
                JobState::Undecided => RunJob::Undecided(job.name),
            };
            inspected.push(entry);
        }
        Ok(InspectedRunJobs {
            jobs: inspected,
            outcomes,
        })
    }

    fn timed_out_cancelled_indices(
        &self,
        jobs: &[WorkflowJob],
        commit: &str,
        workflow_path: Option<&str>,
        has_newer_workflow_run: bool,
    ) -> HashSet<usize> {
        // Check longest-running cancelled jobs first so a timed-out job in a large matrix
        // is always inspected within MAXIMUM_ANNOTATED_JOBS.
        let mut cancelled_indices: Vec<(usize, Option<u64>)> = jobs
            .iter()
            .enumerate()
            .filter(|(_, job)| {
                job.status
                    .as_deref()
                    .is_none_or(|state| state == "completed")
                    && job.conclusion.as_deref() == Some("cancelled")
            })
            .map(|(index, job)| {
                let elapsed =
                    elapsed_seconds(job.started_at.as_deref(), job.completed_at.as_deref());
                (index, elapsed)
            })
            .collect();
        cancelled_indices.sort_by_key(|(_, elapsed)| std::cmp::Reverse(*elapsed));
        let mut workflow_yaml: Option<Option<String>> = None;
        let mut timed_out = HashSet::new();
        for (index, elapsed) in cancelled_indices.into_iter().take(MAXIMUM_ANNOTATED_JOBS) {
            let job = &jobs[index];
            let annotation_texts = self
                .client
                .annotations(job.id)
                .ok()
                .map(annotation_messages);
            let annotation_refs: Option<Vec<&str>> = annotation_texts
                .as_ref()
                .map(|messages| messages.iter().map(String::as_str).collect());
            let timeout_minutes =
                if annotation_refs.is_none() && !has_newer_workflow_run && elapsed.is_some() {
                    let yaml = workflow_yaml.get_or_insert_with(|| {
                        workflow_path
                            .filter(|path| !path.trim().is_empty() && !commit.trim().is_empty())
                            .and_then(|path| self.client.file_at_commit(path, commit).ok())
                    });
                    yaml.as_deref()
                        .and_then(|text| base_ci::workflow_job_timeout_minutes(text, &job.name))
                } else {
                    None
                };
            let evidence = CancelledJobEvidence {
                annotations: annotation_refs.as_deref(),
                has_newer_workflow_run,
                elapsed_seconds: elapsed,
                timeout_minutes,
            };
            if base_ci::cancelled_job_timed_out(&evidence) {
                timed_out.insert(index);
            }
        }
        timed_out
    }

    /// The output of a failure's failed steps and timed-out jobs, also saved whole in the
    /// temporary folder so the repair thread can read past its prompt's excerpt (#584).
    pub fn failure_log(&self, failure: &Failure) -> Result<FailedLog, GitHubCommandError> {
        let has_regular_failure =
            failure.jobs.is_empty() || failure.jobs.iter().any(|job| !job.timed_out || job.id == 0);
        let mut sections: Vec<String> = Vec::new();
        let mut last_error: Option<GitHubCommandError> = None;
        if has_regular_failure {
            match self.client.failed_log(failure.run) {
                Ok(text) if !text.trim().is_empty() => sections.push(text),
                Ok(_) => {}
                Err(error) => last_error = Some(error),
            }
        }
        for job in failure
            .jobs
            .iter()
            .filter(|job| job.timed_out && job.id != 0)
        {
            let already_present = sections
                .iter()
                .any(|section| !base_ci::job_log(section, &job.job).trim().is_empty());
            if already_present {
                continue;
            }
            match self.timed_out_job_log(job) {
                Ok(text) if !text.trim().is_empty() => sections.push(text),
                Ok(_) => {}
                Err(error) => last_error = Some(error),
            }
        }
        if sections.is_empty()
            && let Some(error) = last_error
        {
            return Err(error);
        }
        let text = sections.join("\n");
        let saved = save_failed_log(failure.run, &text);
        Ok(FailedLog { text, saved })
    }

    fn timed_out_job_log(&self, job: &FailedJob) -> Result<String, GitHubCommandError> {
        if let Ok(text) = self.client.job_log(job.id)
            && !base_ci::job_log(&text, &job.job).trim().is_empty()
        {
            return Ok(text);
        }
        let raw = self.client.raw_job_log(job.id)?;
        let step = job.step.as_deref().unwrap_or("job");
        let formatted = raw
            .lines()
            .map(|line| format!("{}\t{step}\t{line}", job.job))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(formatted)
    }
}

struct PreparedRuns {
    runs: Vec<BaseRun>,
    workflow_paths: BTreeMap<u64, String>,
    cancelled_ids: HashSet<u64>,
    completed_stamps: BTreeMap<u64, RunJobsStamp>,
}

fn prepare_workflow_runs(raw_runs: Vec<WorkflowRun>) -> PreparedRuns {
    let mut workflow_paths = BTreeMap::new();
    let mut runs = Vec::with_capacity(raw_runs.len());
    let mut cancelled_ids = HashSet::new();
    let mut completed_stamps = BTreeMap::new();
    for raw in raw_runs {
        if raw.completed() {
            completed_stamps.insert(raw.id, RunJobsStamp::from_workflow_run(&raw));
            if raw.conclusion.as_deref() == Some("cancelled") {
                cancelled_ids.insert(raw.id);
            }
        }
        if let Some(workflow_path) = raw.path.as_deref().filter(|value| !value.trim().is_empty()) {
            workflow_paths.insert(raw.id, workflow_path.to_owned());
        }
        runs.push(base_run(raw));
    }
    PreparedRuns {
        runs,
        workflow_paths,
        cancelled_ids,
        completed_stamps,
    }
}

fn load_run_jobs<F>(
    run_id: u64,
    commit: &str,
    workflow_path: Option<&str>,
    has_newer_workflow_run: bool,
    completed_stamps: &BTreeMap<u64, RunJobsStamp>,
    cache: &mut BaseCiPollCache,
    fetch: &mut F,
) -> Result<InspectedRunJobs, GitHubCommandError>
where
    F: FnMut(u64, &str, Option<&str>, bool) -> Result<InspectedRunJobs, GitHubCommandError>,
{
    let stamp = completed_stamps.get(&run_id);
    if let Some(cached) = cache.completed_jobs(run_id, stamp) {
        return Ok(cached);
    }
    let inspected = fetch(run_id, commit, workflow_path, has_newer_workflow_run)?;
    cache.record_completed_jobs(run_id, stamp, &inspected);
    Ok(inspected)
}

type PromotedRunJobs = (
    BTreeMap<u64, Vec<RunJob>>,
    BTreeMap<u64, Vec<JobPlatformOutcome>>,
);

fn promote_timed_out_runs<F>(
    runs: &mut [BaseRun],
    cancelled_ids: &HashSet<u64>,
    workflow_paths: &BTreeMap<u64, String>,
    completed_stamps: &BTreeMap<u64, RunJobsStamp>,
    cache: &mut BaseCiPollCache,
    fetch: &mut F,
) -> Result<PromotedRunJobs, GitHubCommandError>
where
    F: FnMut(u64, &str, Option<&str>, bool) -> Result<InspectedRunJobs, GitHubCommandError>,
{
    let inputs: Vec<RunCandidateInput<'_>> = runs
        .iter()
        .map(|run| RunCandidateInput {
            workflow: run.identity(),
            run: run.run,
            state: run.state,
            cancelled: cancelled_ids.contains(&run.run),
        })
        .collect();
    let candidates = base_ci::cancelled_candidates(&inputs);
    let mut run_jobs = BTreeMap::new();
    let mut run_job_outcomes = BTreeMap::new();
    let mut resolved_workflows: HashSet<String> = HashSet::new();
    let mut checked_per_workflow: BTreeMap<String, usize> = BTreeMap::new();
    for candidate in candidates {
        if resolved_workflows.contains(&candidate.workflow) {
            continue;
        }
        let checked = checked_per_workflow
            .entry(candidate.workflow.clone())
            .or_insert(0);
        if *checked >= MAX_CANCELLED_CHECKS_PER_WORKFLOW {
            continue;
        }
        *checked += 1;
        let Some(run) = runs.iter_mut().find(|entry| entry.run == candidate.run) else {
            continue;
        };
        let inspected = load_run_jobs(
            run.run,
            &run.commit,
            workflow_paths.get(&run.run).map(String::as_str),
            candidate.has_newer_workflow_run,
            completed_stamps,
            cache,
            fetch,
        )?;
        let has_failure = inspected
            .jobs
            .iter()
            .any(|job| job.state() == JobState::Failed);
        run_jobs.insert(run.run, inspected.jobs);
        run_job_outcomes.insert(run.run, inspected.outcomes);
        if has_failure {
            run.state = RunState::Failed;
            resolved_workflows.insert(candidate.workflow);
        }
    }
    Ok((run_jobs, run_job_outcomes))
}

/// Reads jobs for each workflow's newest failed run, any newer in-progress runs, and
/// optionally passing/watched runs for deploy platform gating (#587, #685).
#[allow(clippy::too_many_arguments)]
fn inspect_relevant_runs<F>(
    runs: &[BaseRun],
    workflow_paths: &BTreeMap<u64, String>,
    completed_stamps: &BTreeMap<u64, RunJobsStamp>,
    include_newest_passed: bool,
    watched_commits: &BTreeSet<String>,
    run_jobs: &mut BTreeMap<u64, Vec<RunJob>>,
    run_job_outcomes: &mut BTreeMap<u64, Vec<JobPlatformOutcome>>,
    cache: &mut BaseCiPollCache,
    fetch: &mut F,
) -> Result<(), GitHubCommandError>
where
    F: FnMut(u64, &str, Option<&str>, bool) -> Result<InspectedRunJobs, GitHubCommandError>,
{
    for run_id in base_ci::runs_to_inspect_for_commits(
        runs,
        MAX_RUNNING_CHECKS_PER_WORKFLOW,
        include_newest_passed,
        watched_commits,
    ) {
        if run_jobs.contains_key(&run_id) {
            continue;
        }
        let Some(run) = runs.iter().find(|entry| entry.run == run_id) else {
            continue;
        };
        let has_newer = runs
            .iter()
            .any(|other| other.identity() == run.identity() && other.run > run.run);
        let inspected = load_run_jobs(
            run.run,
            &run.commit,
            workflow_paths.get(&run.run).map(String::as_str),
            has_newer,
            completed_stamps,
            cache,
            fetch,
        )?;
        run_jobs.insert(run.run, inspected.jobs);
        run_job_outcomes.insert(run.run, inspected.outcomes);
    }
    Ok(())
}

fn has_running_actions_for_tip(runs: &[BaseRun], tip_sha: &str) -> bool {
    let tip_sha = tip_sha.trim();
    !tip_sha.is_empty()
        && runs.iter().any(|run| {
            run.run != 0
                && run.state == RunState::Running
                && run.commit.trim().eq_ignore_ascii_case(tip_sha)
        })
}

fn failed_actions_job_ids(run_jobs: &BTreeMap<u64, Vec<RunJob>>) -> BTreeSet<u64> {
    run_jobs
        .values()
        .flatten()
        .filter_map(|job| match job {
            RunJob::Failed(failed) if failed.id != 0 => Some(failed.id),
            _ => None,
        })
        .collect()
}

/// Standalone GitHub App check runs on the base branch's tip commit (#652), reusing cached
/// check runs when the tip SHA is unchanged and all check runs are already completed (#671).
fn resolve_check_runs<F>(
    repository: &str,
    tip_sha: &str,
    has_running_actions_on_tip: bool,
    actions_job_ids: &BTreeSet<u64>,
    cache: &mut BaseCiPollCache,
    fetch: F,
) -> Result<Vec<BaseRun>, GitHubCommandError>
where
    F: FnOnce() -> Result<(Vec<CheckRunInput>, bool), GitHubCommandError>,
{
    let commit_page = format!("https://github.com/{repository}/commit/{tip_sha}");
    if let Some(cached) = cache.settled_tip_checks(tip_sha, has_running_actions_on_tip) {
        return Ok(base_ci::check_runs_to_base_runs(
            cached,
            tip_sha,
            &commit_page,
            actions_job_ids,
        ));
    }
    let (checks, truncated) = fetch()?;
    cache.record_tip_checks(tip_sha, has_running_actions_on_tip, truncated, &checks);
    Ok(base_ci::check_runs_to_base_runs(
        &checks,
        tip_sha,
        &commit_page,
        actions_job_ids,
    ))
}

fn save_failed_log(run: u64, text: &str) -> Option<PathBuf> {
    let folder = std::env::temp_dir().join(format!("velocity-base-ci-{run}"));
    let file = folder.join("failed-steps.log");
    std::fs::create_dir_all(&folder)
        .and_then(|()| std::fs::write(&file, text))
        .is_ok()
        .then_some(file)
}

fn annotation_messages(annotations: Vec<Annotation>) -> Vec<String> {
    let mut messages = Vec::new();
    for annotation in annotations {
        if let Some(title) = annotation.title.filter(|value| !value.trim().is_empty()) {
            messages.push(title);
        }
        if let Some(message) = annotation.message.filter(|value| !value.trim().is_empty()) {
            messages.push(message);
        }
    }
    messages
}

fn elapsed_seconds(started: Option<&str>, finished: Option<&str>) -> Option<u64> {
    let started = started?.parse::<jiff::Timestamp>().ok()?;
    let finished = finished?.parse::<jiff::Timestamp>().ok()?;
    u64::try_from(finished.duration_since(started).as_secs()).ok()
}

fn base_run(run: WorkflowRun) -> BaseRun {
    let state = base_ci::run_state(run.status.as_deref(), run.conclusion.as_deref());
    let display = base_ci::actions_workflow_name(run.name.as_deref(), run.path.as_deref());
    let workflow_key =
        base_ci::actions_workflow_key(run.workflow_id, run.path.as_deref(), &display);
    BaseRun {
        workflow_key,
        workflow: display,
        run: run.id,
        commit: run.head_sha,
        url: run.html_url,
        state,
        detail: String::new(),
    }
}

fn failed_job(job: WorkflowJob, timed_out: bool) -> FailedJob {
    let step = job
        .steps
        .iter()
        .find(|step| base_ci::job_failed(step.conclusion.as_deref()))
        .or_else(|| {
            timed_out
                .then(|| {
                    job.steps
                        .iter()
                        .find(|step| step.conclusion.as_deref() == Some("cancelled"))
                })
                .flatten()
        })
        .map(|step| step.name.clone());
    FailedJob {
        id: job.id,
        job: job.name,
        step,
        runner: (!job.labels.is_empty()).then(|| job.labels.join(", ")),
        signature: None,
        timed_out,
        hung_test: None,
    }
}

#[derive(Deserialize)]
struct CheckRunsResponse {
    #[serde(default)]
    total_count: usize,
    #[serde(default)]
    check_runs: Vec<ApiCheckRun>,
}

#[derive(Deserialize)]
struct ApiCheckRun {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    head_sha: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    app: Option<ApiCheckRunApp>,
    #[serde(default)]
    output: Option<ApiCheckRunOutput>,
}

#[derive(Deserialize)]
struct ApiCheckRunApp {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    slug: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct ApiCheckRunOutput {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
}

impl ApiCheckRun {
    fn into_input(self) -> CheckRunInput {
        let (app_id, app_slug, app_name) = match self.app {
            Some(app) => (app.id, app.slug, app.name),
            None => (None, None, None),
        };
        let (output_title, output_summary) = match self.output {
            Some(output) => (output.title, output.summary),
            None => (None, None),
        };
        CheckRunInput {
            id: self.id,
            name: self.name,
            head_sha: self.head_sha,
            status: self.status,
            conclusion: self.conclusion,
            app_id,
            app_slug,
            app_name,
            html_url: self.html_url,
            details_url: self.details_url,
            output_title,
            output_summary,
            started_at: self.started_at,
            completed_at: self.completed_at,
        }
    }
}

/// Percent-encodes `text` for a URL. A branch name may hold `/`, which stays as it is in a
/// path and is encoded in a query value.
pub(crate) fn encoded(text: &str, path: bool) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        let plain = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (path && byte == b'/');
        if plain {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use velocity_core::base_ci::{
        self, CheckRunInput, FailedJob, JobPlatformOutcome, JobState, RunJob, RunState,
    };

    use super::{
        ApiCheckRun, BaseCiPollCache, CheckRunsResponse, InspectedRunJobs, PreparedRuns, base_run,
        has_running_actions_for_tip, inspect_relevant_runs, prepare_workflow_runs,
        promote_timed_out_runs, resolve_check_runs,
    };
    use crate::workflow_runs::WorkflowRun;

    fn workflow_run(
        id: u64,
        workflow_id: u64,
        status: &str,
        conclusion: Option<&str>,
        run_attempt: u64,
        updated_at: &str,
    ) -> WorkflowRun {
        WorkflowRun {
            id,
            workflow_id: Some(workflow_id),
            name: Some(format!("Workflow {workflow_id}")),
            run_number: id,
            run_attempt: Some(run_attempt),
            status: Some(status.to_owned()),
            conclusion: conclusion.map(str::to_owned),
            head_sha: format!("{id:040x}"),
            html_url: format!("https://github.com/mESmaC/velocity/actions/runs/{id}"),
            run_started_at: None,
            updated_at: Some(updated_at.to_owned()),
            event: Some("push".to_owned()),
            path: Some(format!(".github/workflows/w{workflow_id}.yml")),
        }
    }

    fn failed_run_job(id: u64, name: &str, timed_out: bool) -> RunJob {
        RunJob::Failed(FailedJob {
            id,
            job: name.to_owned(),
            timed_out,
            ..FailedJob::default()
        })
    }

    struct PollOutcome {
        runs: Vec<base_ci::BaseRun>,
        run_jobs: BTreeMap<u64, Vec<RunJob>>,
        run_job_outcomes: BTreeMap<u64, Vec<JobPlatformOutcome>>,
        fetched: Vec<u64>,
    }

    fn run_cached_job_poll(
        raw_runs: Vec<WorkflowRun>,
        cache: &mut BaseCiPollCache,
        mut fetch: impl FnMut(u64) -> Vec<RunJob>,
    ) -> Result<PollOutcome, crate::GitHubCommandError> {
        run_cached_job_poll_with_options(raw_runs, cache, false, &BTreeSet::new(), |run_id| {
            let jobs = fetch(run_id);
            let outcomes = jobs
                .iter()
                .map(|job| JobPlatformOutcome {
                    job: job.name().to_owned(),
                    state: job.state(),
                    labels: Vec::new(),
                })
                .collect();
            InspectedRunJobs { jobs, outcomes }
        })
    }

    fn run_cached_job_poll_with_options(
        raw_runs: Vec<WorkflowRun>,
        cache: &mut BaseCiPollCache,
        include_newest_passed: bool,
        watched_commits: &BTreeSet<String>,
        mut fetch: impl FnMut(u64) -> InspectedRunJobs,
    ) -> Result<PollOutcome, crate::GitHubCommandError> {
        cache.reset_if_target_changed("mESmaC/collapse.sh", "main");
        let PreparedRuns {
            mut runs,
            workflow_paths,
            cancelled_ids,
            completed_stamps,
        } = prepare_workflow_runs(raw_runs);
        cache.prune_completed_runs(&completed_stamps);
        let mut fetched = Vec::new();
        let mut inspect = |run_id, _commit: &str, _path: Option<&str>, _has_newer| {
            fetched.push(run_id);
            Ok(fetch(run_id))
        };
        let (mut run_jobs, mut run_job_outcomes) = promote_timed_out_runs(
            &mut runs,
            &cancelled_ids,
            &workflow_paths,
            &completed_stamps,
            cache,
            &mut inspect,
        )?;
        inspect_relevant_runs(
            &runs,
            &workflow_paths,
            &completed_stamps,
            include_newest_passed,
            watched_commits,
            &mut run_jobs,
            &mut run_job_outcomes,
            cache,
            &mut inspect,
        )?;
        Ok(PollOutcome {
            runs,
            run_jobs,
            run_job_outcomes,
            fetched,
        })
    }

    // Risk (#671): steady-state BaseCiWatch polls must cache completed failed and cancelled
    // Actions run jobs across polls so they do not trigger gh_rate bursts on every tick.
    #[test]
    fn completed_failed_and_cancelled_run_jobs_are_cached_across_polls()
    -> Result<(), crate::GitHubCommandError> {
        let mut cache = BaseCiPollCache::default();
        let make_runs = || {
            vec![
                workflow_run(
                    100,
                    1,
                    "completed",
                    Some("failure"),
                    1,
                    "2026-04-01T00:00:00Z",
                ),
                workflow_run(
                    200,
                    2,
                    "completed",
                    Some("cancelled"),
                    1,
                    "2026-04-01T00:00:00Z",
                ),
                workflow_run(
                    201,
                    2,
                    "completed",
                    Some("cancelled"),
                    1,
                    "2026-04-01T00:01:00Z",
                ),
            ]
        };
        let job_map = |run_id: u64| match run_id {
            100 => vec![failed_run_job(1001, "linux", false)],
            200 => vec![failed_run_job(2001, "macos", true)],
            _ => vec![RunJob::Undecided("macos".to_owned())],
        };

        let first = run_cached_job_poll(make_runs(), &mut cache, job_map)?;
        assert_eq!(first.fetched, vec![201, 200, 100]);
        assert_eq!(first.runs[1].state, RunState::Failed);
        assert_eq!(first.run_jobs.len(), 3);

        let second = run_cached_job_poll(make_runs(), &mut cache, job_map)?;
        assert!(second.fetched.is_empty());
        assert_eq!(second.runs, first.runs);
        assert_eq!(second.run_jobs, first.run_jobs);
        Ok(())
    }

    // Risk (#671): in-progress runs and eventually-consistent completed runs with running jobs
    // must not be cached prematurely.
    #[test]
    fn in_progress_and_lagging_completed_runs_are_not_cached()
    -> Result<(), crate::GitHubCommandError> {
        let mut cache = BaseCiPollCache::default();
        let running = workflow_run(300, 1, "in_progress", None, 1, "2026-04-01T00:00:00Z");
        let failed = workflow_run(
            299,
            1,
            "completed",
            Some("failure"),
            1,
            "2026-04-01T00:00:00Z",
        );

        let first = run_cached_job_poll(vec![running.clone(), failed.clone()], &mut cache, |_| {
            vec![RunJob::Running("test".to_owned())]
        })?;
        assert_eq!(first.fetched, vec![300, 299]);
        assert!(cache.completed_run_jobs.is_empty());

        let second =
            run_cached_job_poll(vec![running.clone(), failed.clone()], &mut cache, |id| {
                if id == 300 {
                    vec![RunJob::Running("test".to_owned())]
                } else {
                    vec![failed_run_job(2991, "test", false)]
                }
            })?;
        assert_eq!(second.fetched, vec![300, 299]);
        assert!(cache.completed_run_jobs.contains_key(&299));
        assert!(!cache.completed_run_jobs.contains_key(&300));

        let third = run_cached_job_poll(vec![running, failed], &mut cache, |_| {
            vec![RunJob::Running("test".to_owned())]
        })?;
        assert_eq!(third.fetched, vec![300]);
        Ok(())
    }

    // Risk (#671): re-run attempts on the same run ID must invalidate cached jobs and runs
    // outside the current window must be pruned from the cache.
    #[test]
    fn reruns_invalidate_and_aged_out_runs_are_pruned() -> Result<(), crate::GitHubCommandError> {
        let mut cache = BaseCiPollCache::default();
        let attempt_1 = workflow_run(
            299,
            1,
            "completed",
            Some("failure"),
            1,
            "2026-04-01T00:00:00Z",
        );
        let first = run_cached_job_poll(vec![attempt_1], &mut cache, |_| {
            vec![failed_run_job(2991, "test", false)]
        })?;
        assert_eq!(first.fetched, vec![299]);

        let attempt_2 = workflow_run(
            299,
            1,
            "completed",
            Some("failure"),
            2,
            "2026-04-01T00:05:00Z",
        );
        let second = run_cached_job_poll(vec![attempt_2], &mut cache, |_| {
            vec![failed_run_job(2992, "test", false)]
        })?;
        assert_eq!(second.fetched, vec![299]);

        let newer_pass = workflow_run(
            301,
            1,
            "completed",
            Some("success"),
            1,
            "2026-04-01T00:10:00Z",
        );
        let third = run_cached_job_poll(vec![newer_pass], &mut cache, |_| Vec::new())?;
        assert!(third.fetched.is_empty());
        assert!(cache.completed_run_jobs.is_empty());
        Ok(())
    }

    fn standalone_check(sha: &str, status: &str, conclusion: Option<&str>) -> CheckRunInput {
        CheckRunInput {
            id: 77,
            name: "deploy".to_owned(),
            head_sha: sha.to_owned(),
            status: Some(status.to_owned()),
            conclusion: conclusion.map(str::to_owned),
            app_id: Some(24),
            app_slug: Some("external-ci".to_owned()),
            ..CheckRunInput::default()
        }
    }

    // Risk (#671): settled tip check-runs must skip redundant `/commits/{sha}/check-runs`
    // requests while still re-polling pending check runs and active Actions tips.
    #[test]
    fn tip_check_runs_cache_reuses_completed_tip_and_refreshes_pending()
    -> Result<(), crate::GitHubCommandError> {
        let mut cache = BaseCiPollCache::default();
        let sha1 = "a".repeat(40);
        let mut fetches = 0usize;

        let pending = resolve_check_runs(
            "mESmaC/collapse.sh",
            &sha1,
            false,
            &BTreeSet::new(),
            &mut cache,
            || {
                fetches += 1;
                Ok((vec![standalone_check(&sha1, "in_progress", None)], false))
            },
        )?;
        assert_eq!(fetches, 1);
        assert_eq!(pending[0].state, RunState::Running);

        let completed_first = resolve_check_runs(
            "mESmaC/collapse.sh",
            &sha1,
            false,
            &BTreeSet::new(),
            &mut cache,
            || {
                fetches += 1;
                Ok((
                    vec![standalone_check(&sha1, "completed", Some("failure"))],
                    false,
                ))
            },
        )?;
        assert_eq!(fetches, 2);
        assert_eq!(completed_first[0].state, RunState::Failed);

        let completed_cached = resolve_check_runs(
            "mESmaC/collapse.sh",
            &sha1,
            false,
            &BTreeSet::new(),
            &mut cache,
            || {
                fetches += 1;
                Ok((Vec::new(), false))
            },
        )?;
        assert_eq!(fetches, 2);
        assert_eq!(completed_cached, completed_first);

        let running_actions = vec![base_run(workflow_run(
            400,
            1,
            "in_progress",
            None,
            1,
            "2026-04-01T00:00:00Z",
        ))];
        assert!(has_running_actions_for_tip(
            &running_actions,
            &running_actions[0].commit
        ));
        Ok(())
    }

    // Risk (#671): empty check-runs lists must re-check once before settling so late-starting
    // checks after a push are not missed, and moving to a new tip or target must clear cache.
    #[test]
    fn tip_check_runs_cache_confirms_empty_and_resets_on_new_tip_or_target()
    -> Result<(), crate::GitHubCommandError> {
        let mut cache = BaseCiPollCache::default();
        let sha2 = "b".repeat(40);
        let mut fetches = 0usize;

        for expected_fetches in [1usize, 2, 2] {
            let result = resolve_check_runs(
                "mESmaC/collapse.sh",
                &sha2,
                false,
                &BTreeSet::new(),
                &mut cache,
                || {
                    fetches += 1;
                    Ok((Vec::new(), false))
                },
            )?;
            assert_eq!(fetches, expected_fetches);
            assert!(result.is_empty());
        }

        cache.reset_if_target_changed("mESmaC/other", "main");
        let after_reset = resolve_check_runs(
            "mESmaC/other",
            &sha2,
            false,
            &BTreeSet::new(),
            &mut cache,
            || {
                fetches += 1;
                Ok((Vec::new(), false))
            },
        )?;
        assert_eq!(fetches, 3);
        assert!(after_reset.is_empty());
        Ok(())
    }

    // Risk (#652): the check-runs transport decoder must deserialize minimal GitHub App
    // check-run payloads (without optional URL or output fields) and pass them through to
    // base CI while excluding GitHub Actions check runs.
    #[test]
    fn check_runs_response_decodes_standalone_and_skips_actions_checks()
    -> Result<(), serde_json::Error> {
        let commit = "a".repeat(40);
        let payload = format!(
            r#"{{
                "total_count": 2,
                "check_runs": [
                    {{
                        "head_sha": "{commit}",
                        "app": {{"id": 24, "slug": "external-ci"}},
                        "name": "macOS tests",
                        "status": "completed",
                        "conclusion": "failure"
                    }},
                    {{
                        "id": 9001,
                        "head_sha": "{commit}",
                        "app": {{"id": 15368, "slug": "github-actions"}},
                        "name": "macOS tests",
                        "status": "completed",
                        "conclusion": "failure",
                        "html_url": "https://github.com/mESmaC/velocity/actions/runs/77/job/9001"
                    }}
                ]
            }}"#
        );
        let decoded: CheckRunsResponse = serde_json::from_str(&payload)?;
        let inputs: Vec<_> = decoded
            .check_runs
            .into_iter()
            .map(ApiCheckRun::into_input)
            .collect();
        let commit_page = format!("https://github.com/mESmaC/velocity/commit/{commit}");
        let runs =
            base_ci::check_runs_to_base_runs(&inputs, &commit, &commit_page, &BTreeSet::new());
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].workflow, "external-ci · macOS tests");
        assert_eq!(runs[0].state, RunState::Failed);
        assert_eq!(runs[0].commit, commit);
        assert_eq!(runs[0].url, commit_page);
        Ok(())
    }

    // Risk (#653): two distinct GitHub Actions workflow files can share the display name `CI`.
    // `base_run` must assign independent stable identities from `workflow_id` (falling back to
    // workflow `path`) while preserving the display name for the UI and repair prompt.
    #[test]
    fn same_name_workflows_decode_with_distinct_identities_and_preserved_display_names()
    -> Result<(), serde_json::Error> {
        let payload = r#"[
            {
                "id": 101,
                "workflow_id": 11,
                "name": "CI",
                "path": ".github/workflows/macos.yml",
                "status": "completed",
                "conclusion": "failure",
                "head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "html_url": "https://github.com/mESmaC/velocity/actions/runs/101"
            },
            {
                "id": 102,
                "workflow_id": 12,
                "name": "CI",
                "path": ".github/workflows/docs.yml",
                "status": "completed",
                "conclusion": "success",
                "head_sha": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "html_url": "https://github.com/mESmaC/velocity/actions/runs/102"
            },
            {
                "id": 103,
                "name": "CI",
                "path": ".github/workflows/lint.yml",
                "status": "completed",
                "conclusion": "failure",
                "head_sha": "cccccccccccccccccccccccccccccccccccccccc",
                "html_url": "https://github.com/mESmaC/velocity/actions/runs/103"
            }
        ]"#;
        let decoded: Vec<WorkflowRun> = serde_json::from_str(payload)?;
        let runs: Vec<_> = decoded.into_iter().map(base_run).collect();
        assert_eq!(runs[0].workflow, "CI");
        assert_eq!(runs[0].workflow_key, "workflow:11");
        assert_eq!(runs[1].workflow, "CI");
        assert_eq!(runs[1].workflow_key, "workflow:12");
        assert_eq!(runs[2].workflow, "CI");
        assert_eq!(runs[2].workflow_key, "path:.github/workflows/lint.yml");
        assert_ne!(runs[0].identity(), runs[1].identity());
        assert_ne!(runs[0].identity(), runs[2].identity());
        Ok(())
    }

    // Risk (#685): deploy platform gating needs runner labels (`ubuntu-latest`) for passing
    // runs and watched deploy commits, cached across steady-state polls.
    #[test]
    fn passed_run_platform_outcomes_are_inspected_when_requested_and_cached()
    -> Result<(), crate::GitHubCommandError> {
        let mut cache = BaseCiPollCache::default();
        let passing = workflow_run(
            501,
            1,
            "completed",
            Some("success"),
            1,
            "2026-11-22T23:00:00Z",
        );
        let fetch = |_| InspectedRunJobs {
            jobs: vec![RunJob::Passed("Server (ubuntu-latest)".to_owned())],
            outcomes: vec![JobPlatformOutcome {
                job: "Server (ubuntu-latest)".to_owned(),
                state: JobState::Passed,
                labels: vec!["ubuntu-latest".to_owned()],
            }],
        };

        let first = run_cached_job_poll_with_options(
            vec![passing.clone()],
            &mut cache,
            true,
            &BTreeSet::new(),
            fetch,
        )?;
        assert_eq!(first.fetched, vec![501]);
        assert_eq!(
            first.run_job_outcomes.get(&501),
            Some(&vec![JobPlatformOutcome {
                job: "Server (ubuntu-latest)".to_owned(),
                state: JobState::Passed,
                labels: vec!["ubuntu-latest".to_owned()],
            }])
        );

        let second = run_cached_job_poll_with_options(
            vec![passing],
            &mut cache,
            true,
            &BTreeSet::new(),
            fetch,
        )?;
        assert!(second.fetched.is_empty());
        assert_eq!(second.run_job_outcomes, first.run_job_outcomes);
        Ok(())
    }
}
