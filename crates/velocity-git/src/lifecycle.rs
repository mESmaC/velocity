//! Worktree lifecycle state machine and guards (SPEC.md F5, F12).
//!
//! A worktree's path is fixed when it is created. Merging its PR deletes the worktree (and
//! its local branch when enabled). Closing the PR without merging or failing the thread
//! keeps the worktree. Deleting the thread deletes its worktree too. Per-worktree lifecycle
//! transitions never emit `PruneOrphans`; a repo-wide `git worktree prune` runs only from
//! the explicit Prune orphans action.

/// Lifecycle state of a created worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    Working,
    KeptClosed,
    KeptFailed,
    Deleted,
}

/// A tracked worktree record. `path` is fixed when the worktree is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorktreeRecord<Path, Branch> {
    pub path: Path,
    pub branch: Branch,
    pub state: LifecycleState,
}

/// Events that affect a worktree's lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleEvent<Path> {
    /// The worktree's PR merged into `master`.
    PrMerged(bool),
    /// The worktree's PR was closed without merging.
    PrClosedWithoutMerge,
    /// The owning thread failed on an error.
    ThreadFailed,
    /// The user deleted the owning thread (after the 5 s undo window, F12).
    UserDeletedThread,
    /// The user changed the worktree location setting in Settings → Worktrees.
    LocationChanged(Path),
}

/// Git commands that mutate worktrees or branches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitCommand<Path, Branch> {
    /// `git worktree remove <path>` for a single worktree entry.
    RemoveWorktree(Path),
    /// `git branch -D <branch>` for the merged worktree's local branch.
    DeleteBranch(Branch),
    /// Repo-wide `git worktree prune`, allowed only from the Prune orphans button.
    PruneOrphans,
}

impl<Path: Copy, Branch: Copy> WorktreeRecord<Path, Branch> {
    pub fn new(path: Path, branch: Branch) -> Self {
        WorktreeRecord {
            path,
            branch,
            state: LifecycleState::Working,
        }
    }

    pub fn step(&mut self, event: LifecycleEvent<Path>) -> Vec<GitCommand<Path, Branch>> {
        match event {
            LifecycleEvent::PrMerged(delete_local_branch) => match self.state {
                LifecycleState::Deleted => Vec::new(),
                LifecycleState::Working
                | LifecycleState::KeptClosed
                | LifecycleState::KeptFailed => {
                    self.state = LifecycleState::Deleted;
                    if delete_local_branch {
                        Vec::from([
                            GitCommand::RemoveWorktree(self.path),
                            GitCommand::DeleteBranch(self.branch),
                        ])
                    } else {
                        Vec::from([GitCommand::RemoveWorktree(self.path)])
                    }
                }
            },
            LifecycleEvent::PrClosedWithoutMerge => match self.state {
                LifecycleState::Deleted => Vec::new(),
                LifecycleState::Working
                | LifecycleState::KeptClosed
                | LifecycleState::KeptFailed => {
                    self.state = LifecycleState::KeptClosed;
                    Vec::new()
                }
            },
            LifecycleEvent::ThreadFailed => match self.state {
                LifecycleState::Deleted => Vec::new(),
                LifecycleState::Working
                | LifecycleState::KeptClosed
                | LifecycleState::KeptFailed => {
                    self.state = LifecycleState::KeptFailed;
                    Vec::new()
                }
            },
            LifecycleEvent::UserDeletedThread => match self.state {
                LifecycleState::Deleted => Vec::new(),
                LifecycleState::Working
                | LifecycleState::KeptClosed
                | LifecycleState::KeptFailed => {
                    self.state = LifecycleState::Deleted;
                    Vec::from([GitCommand::RemoveWorktree(self.path)])
                }
            },
            LifecycleEvent::LocationChanged(_) => Vec::new(),
        }
    }
}

/// Returns the repo-wide `git worktree prune` command for the Settings → Worktrees
/// **Prune orphans** button.
pub fn prune_orphans_command<Path, Branch>() -> GitCommand<Path, Branch> {
    GitCommand::PruneOrphans
}
