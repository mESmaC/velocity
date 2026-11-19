//! GitHub's side of the base's CI (SPEC.md F5 "Rosen · Base CI", #507): the workflow runs
//! on a Rosen project's mirrored base branch, the commit statuses on its tip, and a failed
//! run's failed jobs and their log. `velocity_core::base_ci` decides what they mean.
//! Everything is REST through `gh`, which keeps working when a swarm has spent the GraphQL
//! budget.

use std::path::PathBuf;

use serde::Deserialize;
use velocity_core::base_ci::{self, BaseRun, FailedJob};

use crate::GitHubCommandError;
use crate::workflow_runs::{WorkflowClient, WorkflowJob, WorkflowRun};

/// How many of the branch's newest runs one look reads. Only the newest finished run of
/// each workflow decides, so this covers several workflows with runs queued behind them.
const RUNS_READ: usize = 50;

/// The `gh` transport for the repository a Rosen project's base is mirrored to.
pub struct BaseCiWatch {
    client: WorkflowClient,
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
    pub fn runs(&self, branch: &str) -> Result<Vec<BaseRun>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Runs {
            workflow_runs: Vec<WorkflowRun>,
        }
        let path = format!(
            "/actions/runs?branch={}&per_page={RUNS_READ}&exclude_pull_requests=true",
            encoded(branch, false)
        );
        let mut runs: Vec<BaseRun> = self
            .client
            .read::<Runs>(&path)?
            .workflow_runs
            .into_iter()
            .filter(|run| run.event.as_deref().is_some_and(base_ci::counts_for_base))
            .map(base_run)
            .collect();
        runs.extend(self.statuses(branch)?);
        Ok(runs)
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

    /// The jobs that failed in the run's latest attempt, each with its first failed step.
    pub fn failed_jobs(&self, run: u64) -> Result<Vec<FailedJob>, GitHubCommandError> {
        Ok(self
            .client
            .jobs(run)?
            .into_iter()
            .filter(|job| base_ci::job_failed(job.conclusion.as_deref()))
            .map(failed_job)
            .collect())
    }

    /// The output of the run's failed steps, also saved whole in the temporary folder so
    /// the thread that fixes it can read past its prompt's excerpt.
    pub fn failed_log(&self, run: u64) -> Result<FailedLog, GitHubCommandError> {
        let text = self.client.failed_log(run)?;
        let folder = std::env::temp_dir().join(format!("velocity-base-ci-{run}"));
        let file = folder.join("failed-steps.log");
        let saved = std::fs::create_dir_all(&folder)
            .and_then(|()| std::fs::write(&file, &text))
            .is_ok()
            .then_some(file);
        Ok(FailedLog { text, saved })
    }
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

fn failed_job(job: WorkflowJob) -> FailedJob {
    let step = job
        .steps
        .iter()
        .find(|step| base_ci::job_failed(step.conclusion.as_deref()))
        .map(|step| step.name.clone());
    FailedJob {
        job: job.name,
        step,
        runner: (!job.labels.is_empty()).then(|| job.labels.join(", ")),
    }
}

/// Percent-encodes `text` for a URL. A branch name may hold `/`, which stays as it is in a
/// path and is encoded in a query value.
fn encoded(text: &str, path: bool) -> String {
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
