//! One durable fix-forward thread per build culprit, retained after thread deletion (#406).

use serde::{Deserialize, Serialize};

use crate::StateStore;
use crate::threads::{NewThread, ThreadError, insert_thread};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRepair {
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub thread: u32,
    #[serde(default)]
    pub closed: bool,
}

impl StateStore {
    /// Allocate the thread, its opening prompt and the culprit identity in one save.
    /// Retaining this association prevents retries and restarts from starting duplicates.
    pub fn add_build_repair(
        &mut self,
        project: u32,
        commit: &str,
        thread: NewThread,
        prompt: &str,
    ) -> Result<Option<u32>, ThreadError> {
        let current = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if current
            .build_repairs
            .iter()
            .any(|repair| repair.commit == commit)
        {
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
        current.active_thread_id = selected;
        current.build_repairs.push(BuildRepair {
            commit: commit.into(),
            thread: identifier,
            closed: false,
        });
        self.commit(next)?;
        Ok(Some(identifier))
    }

    /// Only successful build coverage or a completed inverse settles the culprit record.
    pub fn close_build_repairs(
        &mut self,
        project: u32,
        commits: &[String],
    ) -> Result<(), ThreadError> {
        self.try_edit_project(
            project,
            || ThreadError::UnknownProject(project),
            |current| {
                let mut changed = false;
                for repair in &mut current.build_repairs {
                    if commits.contains(&repair.commit) && !repair.closed {
                        repair.closed = true;
                        changed = true;
                    }
                }
                Ok((changed, ()))
            },
        )
    }
}
