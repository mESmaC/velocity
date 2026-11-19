//! Fix-forward ownership for GitHub CI failures on the base, and the base's CI state as
//! last read (F5 "Rosen · Base CI", #507). One open repair per workflow and job, kept
//! until a green run of that workflow, even when its thread is deleted.

use serde::{Deserialize, Serialize};
use velocity_core::base_ci::{BaseCi, Failure};

use crate::threads::{NewThread, ThreadError, insert_thread};
use crate::{StateStore, StoreError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiRepair {
    /// The workflow's name, or a commit status's context.
    #[serde(default)]
    pub workflow: String,
    #[serde(default)]
    pub job: String,
    /// The base commit and run the failure was first seen on.
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub run: u64,
    #[serde(default)]
    pub thread: u32,
}

impl StateStore {
    /// Allocates the thread, its opening prompt and a repair for each of the failure's jobs
    /// that has none, in one save. Retaining them prevents a repeat of the failure, a
    /// retry or a restart from starting a second thread. `None` when every job is covered.
    pub fn add_ci_repair(
        &mut self,
        project: u32,
        failure: &Failure,
        thread: NewThread,
        prompt: &str,
    ) -> Result<Option<u32>, ThreadError> {
        let current = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let uncovered: Vec<&str> = failure
            .jobs
            .iter()
            .map(|job| job.job.as_str())
            .filter(|job| {
                !current
                    .ci_repairs
                    .iter()
                    .any(|repair| repair.workflow == failure.workflow && repair.job == *job)
            })
            .collect();
        if uncovered.is_empty() {
            return Ok(None);
        }
        let mut next = self.state.clone();
        let selected = current.active_thread_id;
        let identifier = insert_thread(&mut next, project, thread, None)?;
        let current = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if let Some(thread) = current
            .threads
            .iter_mut()
            .find(|thread| thread.id == identifier)
        {
            thread.pending_rosen_launch = Some(prompt.into());
        }
        // A background repair never takes the window from the thread being read.
        current.active_thread_id = selected;
        current
            .ci_repairs
            .extend(uncovered.into_iter().map(|job| CiRepair {
                workflow: failure.workflow.clone(),
                job: job.into(),
                commit: failure.commit.clone(),
                run: failure.run,
                thread: identifier,
            }));
        self.commit(next)?;
        Ok(Some(identifier))
    }

    /// Drops the repairs of `workflows`, whose newest run on the base passed, and returns
    /// them. A later failure of the same job is then a new failure with its own thread.
    pub fn close_ci_repairs(
        &mut self,
        project: u32,
        workflows: &[String],
    ) -> Result<Vec<CiRepair>, StoreError> {
        let mut next = self.state.clone();
        let Some(current) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(Vec::new());
        };
        let (closed, open): (Vec<_>, Vec<_>) = std::mem::take(&mut current.ci_repairs)
            .into_iter()
            .partition(|repair| workflows.contains(&repair.workflow));
        if closed.is_empty() {
            return Ok(closed);
        }
        current.ci_repairs = open;
        self.store_snapshot(next)?;
        Ok(closed)
    }

    /// Saves the base's CI state as last read and reports whether it changed. `None`
    /// clears it, as when the project no longer has a GitHub repository.
    pub fn save_base_ci(&mut self, project: u32, ci: Option<BaseCi>) -> Result<bool, StoreError> {
        let unchanged = self.project(project).is_none_or(|item| item.base_ci == ci);
        if unchanged {
            return Ok(false);
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            item.base_ci = ci;
        }
        self.store_snapshot(next)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use velocity_core::base_ci::{BaseCi, BaseCiState, FailedJob, Failure};

    use crate::threads::NewThread;
    use crate::{ProjectItem, StateStore};

    // Risk (#507): a repair lost on reload starts a second thread for a failure that
    // already has one, and one kept after its workflow turned green hides the next failure.
    #[test]
    fn ci_repairs_and_state_survive_reload_until_a_green_run()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-ci-repairs-{unique}"));
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects = vec![ProjectItem::new(1, "One".into(), r"C:\project one".into())];
        store.store_snapshot(store.state.clone())?;
        let job = |name: &str| FailedJob {
            job: name.into(),
            ..FailedJob::default()
        };
        let mut failure = Failure {
            workflow: "Build".into(),
            run: 7,
            commit: "a".repeat(40),
            url: "https://github.com/owner/one/actions/runs/7".into(),
            detail: String::new(),
            jobs: vec![job("test (macos-latest)")],
        };
        let settings = store.settings();
        let thread = |slug: &str| {
            NewThread::new(
                "Fix CI".into(),
                format!("vel/{slug}"),
                format!(r"C:\worktrees\{slug}"),
                &settings,
            )
        };
        let first = store.add_ci_repair(1, &failure, thread("one"), "prompt")?;
        assert!(first.is_some());
        let failing = BaseCi {
            state: BaseCiState::Failed,
            commit: Some(failure.commit.clone()),
            failing: vec!["Build · test (macos-latest)".into()],
            running: false,
        };
        assert!(store.save_base_ci(1, Some(failing.clone()))?);

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.state.projects[0].base_ci, Some(failing));
        // The same job again is a repeat; a job that newly fails gets its own repair.
        assert_eq!(
            store.add_ci_repair(1, &failure, thread("two"), "prompt")?,
            None
        );
        failure.jobs.push(job("clippy"));
        let second = store.add_ci_repair(1, &failure, thread("three"), "prompt")?;
        assert!(second.is_some() && second != first);
        assert_eq!(store.state.projects[0].ci_repairs.len(), 2);
        assert!(store.close_ci_repairs(1, &["Docs".into()])?.is_empty());
        assert_eq!(store.close_ci_repairs(1, &["Build".into()])?.len(), 2);

        let mut store = StateStore::load_or_default(&directory)?;
        assert!(store.state.projects[0].ci_repairs.is_empty());
        assert!(
            store
                .add_ci_repair(1, &failure, thread("four"), "prompt")?
                .is_some()
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
