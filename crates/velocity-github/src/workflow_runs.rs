//! GitHub's side of a workflow run an agent requested (#508): the dispatch, the run it
//! created, and that run's jobs, annotations and artifacts. Every call is `gh` signed in as
//! the owner, which is why agents ask Velocity instead of running `gh` themselves.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::gh_command::GitHubCommand;
use crate::{GitHubCommandError, RepositoryName};

/// The dispatch option that makes GitHub answer with the run it created.
const RETURN_RUN_DETAILS: &str = "return_run_details";
/// How long a dispatched run may take to be listed. GitHub lists one within seconds.
const APPEAR_LIMIT: Duration = Duration::from_secs(90);
const APPEAR_POLL: Duration = Duration::from_secs(3);

/// One dispatch at a time across this app's sessions. Without the run details option the
/// new run is told apart only by not having been listed before, so two agents dispatching
/// one workflow together could otherwise each be handed the other's run.
static DISPATCHING: Mutex<()> = Mutex::new(());

pub(crate) struct WorkflowClient {
    command: GitHubCommand,
    repository: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct WorkflowRun {
    pub id: u64,
    /// GitHub's immutable workflow ID, which distinguishes distinct workflows sharing a name (#653).
    #[serde(default)]
    pub workflow_id: Option<u64>,
    pub name: Option<String>,
    #[serde(default)]
    pub run_number: u64,
    /// GitHub's attempt counter for this run, incremented when a run is re-run (#671).
    #[serde(default)]
    pub run_attempt: Option<u64>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    #[serde(default)]
    pub head_sha: String,
    pub html_url: String,
    pub run_started_at: Option<String>,
    pub updated_at: Option<String>,
    /// What started the run, such as `push` or `workflow_dispatch`.
    #[serde(default)]
    pub event: Option<String>,
    /// The workflow's file, which names a workflow that has no name of its own.
    #[serde(default)]
    pub path: Option<String>,
}

impl WorkflowRun {
    pub fn completed(&self) -> bool {
        self.status.as_deref() == Some("completed")
    }
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct WorkflowJob {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub html_url: Option<String>,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
    /// The runner labels the job asked for, such as `macos-latest`.
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct WorkflowStep {
    pub name: String,
    pub conclusion: Option<String>,
}

/// A message a job attached to its run: an error, a warning or a notice.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Annotation {
    pub path: Option<String>,
    pub start_line: Option<u64>,
    pub annotation_level: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub message: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Artifact {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub size_in_bytes: u64,
    #[serde(default)]
    pub expired: bool,
}

impl WorkflowClient {
    /// The client for the repository a Rosen project mirrors to. No request is made here.
    #[track_caller]
    pub fn for_mirror(remote: &str) -> Result<Self, GitHubCommandError> {
        let remote = remote.trim();
        let path = ["git@github.com:", "ssh://git@github.com/"]
            .iter()
            .find_map(|prefix| remote.strip_prefix(prefix))
            .unwrap_or(remote);
        let repository = RepositoryName::parse(path).map_err(|_| {
            GitHubCommandError::Invalid(format!(
                "The project's mirror {remote} is not a GitHub repository"
            ))
        })?;
        Ok(Self {
            command: GitHubCommand::for_repository(&repository),
            repository: repository.full_name(),
        })
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub(crate) fn read<T: DeserializeOwned>(&self, path: &str) -> Result<T, GitHubCommandError> {
        let endpoint = format!("repos/{}{path}", self.repository);
        self.command.read(&["api", &endpoint])
    }

    pub fn default_branch(&self) -> Result<String, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Repository {
            default_branch: String,
        }
        Ok(self.read::<Repository>("")?.default_branch)
    }

    /// Starts `workflow` at `reference` and returns the run GitHub created for it.
    pub fn dispatch(
        &self,
        workflow: &str,
        reference: &str,
        inputs: &BTreeMap<String, String>,
        stopped: &dyn Fn() -> bool,
    ) -> Result<WorkflowRun, GitHubCommandError> {
        let _dispatching = DISPATCHING.lock().unwrap_or_else(PoisonError::into_inner);
        let before: BTreeSet<u64> = self
            .dispatched_runs(workflow)?
            .iter()
            .map(|run| run.id)
            .collect();
        match self.request_dispatch(workflow, reference, inputs)? {
            Some(run) => self.run(run),
            None => self.appeared(workflow, &before, stopped),
        }
    }

    /// The run's ID when GitHub reports it with the dispatch, `None` when it only accepts.
    fn request_dispatch(
        &self,
        workflow: &str,
        reference: &str,
        inputs: &BTreeMap<String, String>,
    ) -> Result<Option<u64>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Created {
            workflow_run_id: Option<u64>,
        }
        let endpoint = format!(
            "repos/{}/actions/workflows/{workflow}/dispatches",
            self.repository
        );
        let mut body = json!({"ref": reference, "inputs": inputs});
        body[RETURN_RUN_DETAILS] = json!(true);
        let answer = match self.command.answer("POST", &endpoint, &body) {
            // A GitHub that does not know the option names it in its refusal and starts
            // nothing, so the same dispatch is asked again without it.
            Err(GitHubCommandError::Command { message, .. })
                if message.contains(RETURN_RUN_DETAILS) =>
            {
                let plain = json!({"ref": reference, "inputs": inputs});
                self.command.answer("POST", &endpoint, &plain)?
            }
            answer => answer?,
        };
        Ok(serde_json::from_slice::<Created>(&answer)
            .ok()
            .and_then(|created| created.workflow_run_id))
    }

    /// The run a dispatch created: the oldest one listed now that was not listed `before`.
    fn appeared(
        &self,
        workflow: &str,
        before: &BTreeSet<u64>,
        stopped: &dyn Fn() -> bool,
    ) -> Result<WorkflowRun, GitHubCommandError> {
        let started = Instant::now();
        loop {
            // A failed listing is retried: the dispatch is already made, and giving up
            // here would lose the run.
            let created = self
                .dispatched_runs(workflow)
                .unwrap_or_default()
                .into_iter()
                .filter(|run| !before.contains(&run.id))
                .min_by_key(|run| run.id);
            if let Some(run) = created {
                return Ok(run);
            }
            if stopped() || started.elapsed() > APPEAR_LIMIT {
                return Err(GitHubCommandError::Invalid(format!(
                    "GitHub accepted the dispatch of {workflow} but listed no new run for it. See https://github.com/{}/actions/workflows/{workflow}",
                    self.repository
                )));
            }
            std::thread::sleep(APPEAR_POLL);
        }
    }

    fn dispatched_runs(&self, workflow: &str) -> Result<Vec<WorkflowRun>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Runs {
            workflow_runs: Vec<WorkflowRun>,
        }
        let path =
            format!("/actions/workflows/{workflow}/runs?event=workflow_dispatch&per_page=30");
        Ok(self.read::<Runs>(&path)?.workflow_runs)
    }

    pub fn run(&self, run: u64) -> Result<WorkflowRun, GitHubCommandError> {
        self.read(&format!("/actions/runs/{run}"))
    }

    /// The jobs of the run's latest attempt, following every page so large matrices are not
    /// truncated at GitHub's 100-job page limit (#654).
    pub fn jobs(&self, run: u64) -> Result<Vec<WorkflowJob>, GitHubCommandError> {
        collect_job_pages(|page| {
            let path = format!(
                "/actions/runs/{run}/jobs?filter=latest&per_page={JOBS_PER_PAGE}&page={page}"
            );
            self.read(&path)
        })
    }

    /// A job is also a check run, and its annotations are filed under that.
    pub fn annotations(&self, job: u64) -> Result<Vec<Annotation>, GitHubCommandError> {
        self.read(&format!("/check-runs/{job}/annotations?per_page=50"))
    }

    pub fn artifacts(&self, run: u64) -> Result<Vec<Artifact>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Artifacts {
            artifacts: Vec<Artifact>,
        }
        let path = format!("/actions/runs/{run}/artifacts?per_page=100");
        Ok(self.read::<Artifacts>(&path)?.artifacts)
    }

    /// Unpacks the artifact called `name` into `folder`.
    pub fn download_artifact(
        &self,
        run: u64,
        name: &str,
        folder: &Path,
    ) -> Result<(), GitHubCommandError> {
        let run = run.to_string();
        let folder = folder.to_string_lossy();
        self.command.text(&[
            "run",
            "download",
            &run,
            "--repo",
            &self.repository,
            "--name",
            name,
            "--dir",
            &folder,
        ])?;
        Ok(())
    }

    /// The output of the run's failed steps, one line per log line.
    pub fn failed_log(&self, run: u64) -> Result<String, GitHubCommandError> {
        let run = run.to_string();
        self.command.text(&[
            "run",
            "view",
            &run,
            "--repo",
            &self.repository,
            "--log-failed",
        ])
    }

    /// The full tab-separated log of a single job (`job\tstep\tline`), used when a timed-out
    /// job concluded `cancelled` and `--log-failed` has no failed step (#584).
    pub fn job_log(&self, job: u64) -> Result<String, GitHubCommandError> {
        let job = job.to_string();
        self.command.text(&[
            "run",
            "view",
            "--job",
            &job,
            "--repo",
            &self.repository,
            "--log",
        ])
    }

    /// Raw job log text from GitHub's job-logs REST endpoint, used as a fallback when
    /// `gh run view --job --log` returns no output for a cancelled job (#584).
    pub fn raw_job_log(&self, job: u64) -> Result<String, GitHubCommandError> {
        let endpoint = format!("repos/{}/actions/jobs/{job}/logs", self.repository);
        self.command.text(&["api", &endpoint])
    }

    /// Reads a repository file as raw UTF-8 text at `commit` (for workflow `timeout-minutes`
    /// lookup when check-run annotations cannot be read, #584).
    pub fn file_at_commit(&self, path: &str, commit: &str) -> Result<String, GitHubCommandError> {
        let endpoint = format!(
            "repos/{}/contents/{}?ref={}",
            self.repository,
            crate::base_ci::encoded(path.trim_start_matches('/'), true),
            crate::base_ci::encoded(commit, false),
        );
        self.command
            .text(&["api", "-H", "Accept: application/vnd.github.raw", &endpoint])
    }
}

/// GitHub's maximum `per_page` for `/actions/runs/{run}/jobs` (#654).
const JOBS_PER_PAGE: usize = 100;
/// Cap on job pages read for one workflow run so a misbehaving server cannot loop forever (#654).
const MAX_JOB_PAGES: usize = 50;

#[derive(Deserialize)]
struct JobsPage {
    #[serde(default)]
    total_count: usize,
    #[serde(default)]
    jobs: Vec<WorkflowJob>,
}

/// Collects every page of jobs for a workflow run's latest attempt (#654).
///
/// Deduplicates non-zero job IDs in case jobs shift across pages while a run is still in
/// progress, and stops once a page is short, yields no new jobs, or reaches `total_count`.
fn collect_job_pages<F>(mut read_page: F) -> Result<Vec<WorkflowJob>, GitHubCommandError>
where
    F: FnMut(usize) -> Result<JobsPage, GitHubCommandError>,
{
    let mut jobs = Vec::new();
    let mut seen_ids: BTreeMap<u64, usize> = BTreeMap::new();
    for page in 1..=MAX_JOB_PAGES {
        let response = read_page(page)?;
        let received = response.jobs.len();
        let before = jobs.len();
        for job in response.jobs {
            if job.id != 0
                && let Some(&index) = seen_ids.get(&job.id)
            {
                jobs[index] = job;
                continue;
            }
            if job.id != 0 {
                seen_ids.insert(job.id, jobs.len());
            }
            jobs.push(job);
        }
        let exhausted_total = response.total_count > 0 && jobs.len() >= response.total_count;
        let no_progress = page > 1 && jobs.len() == before;
        if received < JOBS_PER_PAGE || exhausted_total || no_progress {
            break;
        }
    }
    Ok(jobs)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use velocity_core::base_ci::{
        self, BaseCiState, BaseRun, FailedJob, JobState, RunJob, RunState,
    };

    use super::{GitHubCommandError, JobsPage, WorkflowJob, collect_job_pages};

    fn passing_job_json(number: u64) -> serde_json::Value {
        json!({
            "id": number,
            "name": format!("test (shard {number})"),
            "status": "completed",
            "conclusion": "success",
            "steps": [{"name": "Run tests", "conclusion": "success"}],
            "labels": ["ubuntu-latest"]
        })
    }

    fn to_run_job(job: WorkflowJob) -> RunJob {
        match base_ci::job_state(job.status.as_deref(), job.conclusion.as_deref(), false) {
            JobState::Passed => RunJob::Passed(job.name),
            JobState::Failed => {
                let step = job
                    .steps
                    .iter()
                    .find(|step| base_ci::job_failed(step.conclusion.as_deref()))
                    .map(|step| step.name.clone());
                RunJob::Failed(FailedJob {
                    id: job.id,
                    job: job.name,
                    step,
                    runner: (!job.labels.is_empty()).then(|| job.labels.join(", ")),
                    signature: None,
                    timed_out: base_ci::job_timed_out_directly(job.conclusion.as_deref()),
                    hung_test: None,
                })
            }
            JobState::Running => RunJob::Running(job.name),
            JobState::Undecided => RunJob::Undecided(job.name),
        }
    }

    // Risk (#654): a failed workflow with 100 passing jobs on page 1 and a failing macOS job on
    // page 2 must follow all pages so the failing job and step reach both the repair planner and
    // the base CI state instead of falling back to a generic whole-run failure.
    #[test]
    fn jobs_pagination_collects_failing_job_on_page_two_for_base_ci_and_repair()
    -> Result<(), GitHubCommandError> {
        let first_page = json!({
            "total_count": 101,
            "jobs": (1..=100).map(passing_job_json).collect::<Vec<_>>()
        });
        let second_page = json!({
            "total_count": 101,
            "jobs": [{
                "id": 101,
                "name": "test (macOS, shard 101)",
                "status": "completed",
                "conclusion": "failure",
                "steps": [{"name": "Run tests", "conclusion": "failure"}],
                "labels": ["macos-latest"]
            }]
        });
        let mut requested_pages = Vec::new();
        let jobs = collect_job_pages(|page| {
            requested_pages.push(page);
            let payload = match page {
                1 => &first_page,
                2 => &second_page,
                other => {
                    return Err(GitHubCommandError::Invalid(format!(
                        "unexpected page {other}"
                    )));
                }
            };
            Ok(serde_json::from_value::<JobsPage>(payload.clone())?)
        })?;
        assert_eq!(requested_pages, vec![1, 2]);
        assert_eq!(jobs.len(), 101);

        let run = BaseRun {
            workflow_key: String::new(),
            workflow: "CI".into(),
            run: 507,
            commit: "a".repeat(40),
            url: "https://github.com/mESmaC/velocity/actions/runs/507".into(),
            state: RunState::Failed,
            detail: String::new(),
        };
        let run_jobs = BTreeMap::from([(run.run, jobs.into_iter().map(to_run_job).collect())]);
        let verdict = base_ci::evaluate_jobs(&[run], &run_jobs);
        let plan = base_ci::plan_for_verdict(&[], &verdict);
        let view = base_ci::view_for_verdict(&verdict);

        assert_eq!(plan.start.len(), 1);
        assert_eq!(plan.start[0].jobs.len(), 1);
        assert_eq!(plan.start[0].jobs[0].job, "test (macOS, shard 101)");
        assert_eq!(plan.start[0].jobs[0].step.as_deref(), Some("Run tests"));
        assert_eq!(
            plan.start[0].jobs[0].runner.as_deref(),
            Some("macos-latest")
        );
        assert_eq!(view.state, BaseCiState::Failed);
        assert_eq!(view.failing, vec!["CI · test (macOS, shard 101)"]);
        Ok(())
    }

    // Risk (#654): exact-100 runs must stop at `total_count` without an extra request, and jobs
    // shifted across pages while a run is in progress must deduplicate by job ID.
    #[test]
    fn jobs_pagination_stops_at_total_count_and_deduplicates_shifted_jobs()
    -> Result<(), GitHubCommandError> {
        let exact_page = json!({
            "total_count": 100,
            "jobs": (1..=100).map(passing_job_json).collect::<Vec<_>>()
        });
        let mut exact_pages = Vec::new();
        let exact = collect_job_pages(|page| {
            exact_pages.push(page);
            Ok(serde_json::from_value::<JobsPage>(exact_page.clone())?)
        })?;
        assert_eq!(exact_pages, vec![1]);
        assert_eq!(exact.len(), 100);

        let shifted_page_one = json!({
            "total_count": 101,
            "jobs": (1..=100).map(passing_job_json).collect::<Vec<_>>()
        });
        let shifted_page_two = json!({
            "total_count": 101,
            "jobs": [
                {
                    "id": 100,
                    "name": "test (shard 100)",
                    "status": "completed",
                    "conclusion": "failure",
                    "steps": [{"name": "Run tests", "conclusion": "failure"}],
                    "labels": ["ubuntu-latest"]
                },
                passing_job_json(101)
            ]
        });
        let mut shifted_pages = Vec::new();
        let shifted = collect_job_pages(|page| {
            shifted_pages.push(page);
            let payload = match page {
                1 => &shifted_page_one,
                2 => &shifted_page_two,
                other => {
                    return Err(GitHubCommandError::Invalid(format!(
                        "unexpected page {other}"
                    )));
                }
            };
            Ok(serde_json::from_value::<JobsPage>(payload.clone())?)
        })?;
        assert_eq!(shifted_pages, vec![1, 2]);
        assert_eq!(shifted.len(), 101);
        assert_eq!(shifted[99].id, 100);
        assert_eq!(shifted[99].conclusion.as_deref(), Some("failure"));
        Ok(())
    }
}
