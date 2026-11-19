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
    pub name: Option<String>,
    #[serde(default)]
    pub run_number: u64,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    #[serde(default)]
    pub head_sha: String,
    pub html_url: String,
    pub run_started_at: Option<String>,
    pub updated_at: Option<String>,
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
    pub conclusion: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub html_url: Option<String>,
    #[serde(default)]
    pub steps: Vec<WorkflowStep>,
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

    fn read<T: DeserializeOwned>(&self, path: &str) -> Result<T, GitHubCommandError> {
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

    /// The jobs of the run's latest attempt.
    pub fn jobs(&self, run: u64) -> Result<Vec<WorkflowJob>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Jobs {
            jobs: Vec<WorkflowJob>,
        }
        let path = format!("/actions/runs/{run}/jobs?per_page=100");
        Ok(self.read::<Jobs>(&path)?.jobs)
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
}
