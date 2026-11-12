//! Worktrees of deleted threads that could not be removed yet (SPEC.md F5, F12).
//!
//! Deleting a thread always drops its record. When its worktree cannot go at that moment
//! (the stopped agent still holds a file, or verification keeps it), it is remembered here
//! in the same write that drops the thread, and a background pass retries it. Saving it
//! means a restart never forgets a worktree that is still on disk.

use serde::{Deserialize, Serialize};

use crate::threads::{ThreadError, remove_thread_record};
use crate::{StateStore, ThreadItem};

/// A deleted thread's worktree that is still on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedWorktree {
    /// The deleted thread, whose agent may still be exiting.
    pub thread: u32,
    /// The thread's fixed worktree path.
    pub path: String,
    /// The thread's saved branch, which removal verifies before it deletes anything.
    pub branch: String,
}

impl DeletedWorktree {
    pub fn new(thread: u32, path: String, branch: String) -> Self {
        Self {
            thread,
            path,
            branch,
        }
    }
}

/// One kept worktree, with the project checkout it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptWorktree {
    pub project: u32,
    pub repository: String,
    pub worktree: DeletedWorktree,
    /// Another saved thread now records this path, so it is no longer the deleted
    /// thread's to remove.
    pub reused: bool,
}

impl StateStore {
    /// Drops a deleted thread's record (as [`StateStore::remove_thread`] does) and, in the
    /// same write, remembers its worktree when it is still on disk.
    pub fn remove_deleted_thread(
        &mut self,
        project: u32,
        thread: u32,
        kept: Option<DeletedWorktree>,
    ) -> Result<ThreadItem, ThreadError> {
        let mut next = self.state.clone();
        let removed = remove_thread_record(&mut next, project, thread)?;
        if let Some(kept) = kept
            && let Some(item) = next.projects.iter_mut().find(|item| item.id == project)
        {
            item.deleted_worktrees
                .retain(|entry| entry.path != kept.path);
            item.deleted_worktrees.push(kept);
        }
        self.commit(next)?;
        Ok(removed)
    }

    /// Every deleted thread's worktree still waiting to be removed.
    pub fn kept_worktrees(&self) -> Vec<KeptWorktree> {
        self.state
            .projects
            .iter()
            .flat_map(|project| {
                project
                    .deleted_worktrees
                    .iter()
                    .map(|worktree| KeptWorktree {
                        project: project.id,
                        repository: project.repository_path.clone(),
                        worktree: worktree.clone(),
                        reused: project
                            .threads
                            .iter()
                            .any(|thread| thread.worktree_path.as_deref() == Some(&worktree.path)),
                    })
            })
            .collect()
    }

    /// Stops retrying a worktree: it is gone, or its path belongs to a live thread now.
    pub fn forget_deleted_worktree(
        &mut self,
        project: u32,
        path: &str,
    ) -> Result<bool, ThreadError> {
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(false);
        };
        let before = item.deleted_worktrees.len();
        item.deleted_worktrees.retain(|entry| entry.path != path);
        if item.deleted_worktrees.len() == before {
            return Ok(false);
        }
        self.commit(next)?;
        Ok(true)
    }
}
