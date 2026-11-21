//! GitHub's side of the base's CI (SPEC.md F5 "Rosen · Base CI", #507): the workflow runs
//! on a Rosen project's mirrored base branch, the commit statuses on its tip, and a failed
//! run's failed jobs and their log. `velocity_core::base_ci` decides what they mean.
//! Everything is REST through `gh`, which keeps working when a swarm has spent the GraphQL
//! budget.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use serde::Deserialize;
use velocity_core::base_ci::{
    self, BaseRun, CancelledJobEvidence, FailedJob, Failure, JobState, RunCandidateInput, RunJob,
    RunState,
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

/// The `gh` transport for the repository a Rosen project's base is mirrored to.
pub struct BaseCiWatch {
    client: WorkflowClient,
}

/// Runs on the base branch together with inspected per-run jobs, avoiding redundant GitHub
/// requests in the same look (#584, #587).
#[derive(Clone, Debug, Default)]
pub struct BaseRunSnapshot {
    pub runs: Vec<BaseRun>,
    pub run_jobs: BTreeMap<u64, Vec<RunJob>>,
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
    pub fn runs(&self, branch: &str) -> Result<BaseRunSnapshot, GitHubCommandError> {
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
        let mut workflow_paths = BTreeMap::new();
        let mut runs = Vec::with_capacity(raw_runs.len());
        let mut cancelled_ids = HashSet::new();
        for raw in raw_runs {
            if raw.status.as_deref() == Some("completed")
                && raw.conclusion.as_deref() == Some("cancelled")
            {
                cancelled_ids.insert(raw.id);
            }
            if let Some(workflow_path) =
                raw.path.as_deref().filter(|value| !value.trim().is_empty())
            {
                workflow_paths.insert(raw.id, workflow_path.to_owned());
            }
            runs.push(base_run(raw));
        }
        let mut run_jobs =
            self.promote_timed_out_runs(&mut runs, &cancelled_ids, &workflow_paths)?;
        self.inspect_relevant_runs(&runs, &workflow_paths, &mut run_jobs)?;
        runs.extend(self.statuses(branch)?);
        Ok(BaseRunSnapshot { runs, run_jobs })
    }

    fn promote_timed_out_runs(
        &self,
        runs: &mut [BaseRun],
        cancelled_ids: &HashSet<u64>,
        workflow_paths: &BTreeMap<u64, String>,
    ) -> Result<BTreeMap<u64, Vec<RunJob>>, GitHubCommandError> {
        let inputs: Vec<RunCandidateInput<'_>> = runs
            .iter()
            .map(|run| RunCandidateInput {
                workflow: &run.workflow,
                run: run.run,
                state: run.state,
                cancelled: cancelled_ids.contains(&run.run),
            })
            .collect();
        let candidates = base_ci::cancelled_candidates(&inputs);
        let mut run_jobs = BTreeMap::new();
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
            let jobs = self.inspect_run_jobs(
                run.run,
                &run.commit,
                workflow_paths.get(&run.run).map(String::as_str),
                candidate.has_newer_workflow_run,
            )?;
            let has_failure = jobs.iter().any(|job| job.state() == JobState::Failed);
            run_jobs.insert(run.run, jobs);
            if has_failure {
                run.state = RunState::Failed;
                resolved_workflows.insert(candidate.workflow);
            }
        }
        Ok(run_jobs)
    }

    /// Reads jobs for each workflow's newest failed run and any newer in-progress runs so
    /// each job is decided by the newest run in which that job finished (#587).
    fn inspect_relevant_runs(
        &self,
        runs: &[BaseRun],
        workflow_paths: &BTreeMap<u64, String>,
        run_jobs: &mut BTreeMap<u64, Vec<RunJob>>,
    ) -> Result<(), GitHubCommandError> {
        for run_id in base_ci::runs_to_inspect(runs, MAX_RUNNING_CHECKS_PER_WORKFLOW) {
            if run_jobs.contains_key(&run_id) {
                continue;
            }
            let Some(run) = runs.iter().find(|entry| entry.run == run_id) else {
                continue;
            };
            let has_newer = runs
                .iter()
                .any(|other| other.workflow == run.workflow && other.run > run.run);
            let jobs = self.inspect_run_jobs(
                run.run,
                &run.commit,
                workflow_paths.get(&run.run).map(String::as_str),
                has_newer,
            )?;
            run_jobs.insert(run.run, jobs);
        }
        Ok(())
    }

    /// The tip's commit statuses, which is how CI other than GitHub Actions reports.
    /// GitHub's combined status holds each context's newest status.
    fn statuses(&self, branch: &str) -> Result<Vec<BaseRun>, GitHubCommandError> {
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
        Ok(combined
            .statuses
            .into_iter()
            .filter(|status| status.context != base_ci::INTEGRATION_CONTEXT)
            .map(|status| BaseRun {
                workflow: status.context,
                run: 0,
                commit: combined.sha.clone(),
                url: status.target_url.unwrap_or_else(|| commit_page.clone()),
                state: base_ci::status_state(&status.state),
                detail: status.description.unwrap_or_default(),
            })
            .collect())
    }

    fn inspect_run_jobs(
        &self,
        run: u64,
        commit: &str,
        workflow_path: Option<&str>,
        has_newer_workflow_run: bool,
    ) -> Result<Vec<RunJob>, GitHubCommandError> {
        let jobs = self.client.jobs(run)?;
        let timed_out_cancelled =
            self.timed_out_cancelled_indices(&jobs, commit, workflow_path, has_newer_workflow_run);
        let mut inspected = Vec::with_capacity(jobs.len());
        for (index, job) in jobs.into_iter().enumerate() {
            let timed_out_cancelled = timed_out_cancelled.contains(&index);
            let state = base_ci::job_state(
                job.status.as_deref(),
                job.conclusion.as_deref(),
                timed_out_cancelled,
            );
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
        Ok(inspected)
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
    let workflow = run
        .name
        .filter(|name| !name.trim().is_empty())
        .or(run.path)
        .unwrap_or_else(|| "workflow".into());
    BaseRun {
        workflow,
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
