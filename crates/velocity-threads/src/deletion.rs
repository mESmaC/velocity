//! Deleting a thread (SPEC.md F12).
//!
//! Delete is immediate with no confirmation: the thread leaves the sidebar and a toast
//! offers Undo for 5 s. Nothing happens to the agent, worktree, PR or saved state during
//! those seconds, so Undo restores everything exactly. When they are up, the deletion lands:
//! the agent stops, the worktree is removed, any open PR is closed, and the thread's tasks go
//! back to todo as its record leaves `state.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use thiserror::Error;
use velocity_core::thread_menu::{has_open_pull_request, owns_worktree};
use velocity_git::{GitCommand, LifecycleEvent, LifecycleState, WorktreeError, WorktreeRecord};
use velocity_github::{PullRequestClient, PullRequestError, PullRequestOperation};
use velocity_store::{ThreadItem, ThreadState};

use crate::runtime::ThreadKey;

/// How long Undo stays on offer (F12).
pub const UNDO_WINDOW: Duration = Duration::from_secs(5);

/// How long a stopped agent gets to exit before its worktree is removed anyway. Its process
/// holds the worktree as its working directory, which Windows refuses to delete.
pub const AGENT_EXIT_WAIT: Duration = Duration::from_secs(10);

/// Removal attempts while the agent's last processes let go of the worktree, each after
/// `git`'s own short retries.
const REMOVE_ATTEMPTS: u32 = 5;
const REMOVE_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum DeletionError {
    #[error("Could not remove the worktree {path}: {source}")]
    Worktree {
        path: String,
        #[source]
        source: Box<WorktreeError>,
    },
    #[error("Could not remove the worktree {path}: {source}")]
    Folder {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Refusing to remove {0}: it is the project's own checkout")]
    MainCheckout(String),
    #[error("Could not close PR #{number}: {source}")]
    PullRequest {
        number: u32,
        #[source]
        source: Box<PullRequestError>,
    },
}

/// Deletions inside their undo window. Undo and landing each take the deletion out, so
/// whichever comes first wins and the other finds nothing.
#[derive(Debug, Default)]
pub struct PendingDeletions {
    next: u32,
    pending: BTreeMap<u32, ThreadKey>,
}

impl PendingDeletions {
    /// Starts the undo window for a thread and returns the deletion's token.
    pub fn begin(&mut self, key: ThreadKey) -> u32 {
        self.next = self.next.wrapping_add(1);
        self.pending.insert(self.next, key);
        self.next
    }

    pub fn is_pending(&self, key: ThreadKey) -> bool {
        self.pending.values().any(|pending| *pending == key)
    }

    /// Undo: the thread comes back untouched.
    pub fn undo(&mut self, token: u32) -> Option<ThreadKey> {
        self.pending.remove(&token)
    }

    /// The window is over: the deletion lands unless it was undone.
    pub fn land(&mut self, token: u32) -> Option<ThreadKey> {
        self.pending.remove(&token)
    }
}

/// What landing a deletion does besides stopping the agent and dropping the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletionPlan {
    pub repository: PathBuf,
    pub remove_worktree: Option<PathBuf>,
    pub close_pull_request: Option<u32>,
}

/// Plans the deletion of `thread`, whose project is checked out at `repository`. The
/// worktree goes through the worktree lifecycle's `UserDeletedThread` step (F5, F17).
pub fn plan_deletion(repository: &Path, thread: &ThreadItem) -> DeletionPlan {
    let state = thread.state.into();
    let remove_worktree = thread
        .worktree_path
        .as_deref()
        .filter(|path| owns_worktree(state, Some(path)))
        .and_then(|path| {
            let mut record = WorktreeRecord::new(Path::new(path), thread.branch_name.as_str());
            record.state = lifecycle_state(thread.state);
            record
                .step(LifecycleEvent::UserDeletedThread)
                .into_iter()
                .find_map(|command| match command {
                    GitCommand::RemoveWorktree(path) => Some(path.to_path_buf()),
                    _ => None,
                })
        });
    DeletionPlan {
        repository: repository.to_path_buf(),
        remove_worktree,
        close_pull_request: thread
            .pull_request_number
            .filter(|_| has_open_pull_request(state, thread.pull_request_number)),
    }
}

fn lifecycle_state(state: ThreadState) -> LifecycleState {
    match state {
        ThreadState::Closed => LifecycleState::KeptClosed,
        ThreadState::Error => LifecycleState::KeptFailed,
        ThreadState::Merged => LifecycleState::Deleted,
        _ => LifecycleState::Working,
    }
}

/// Removes the worktree from git and disk, retrying while the stopped agent exits.
pub fn remove_worktree(repository: &Path, worktree: &Path) -> Result<(), DeletionError> {
    let display = worktree.display().to_string();
    let same = velocity_git::inventory::worktree_identity(repository)
        .zip(velocity_git::inventory::worktree_identity(worktree))
        .is_some_and(|(main, target)| main == target);
    if same {
        return Err(DeletionError::MainCheckout(display));
    }
    let command = GitCommand::RemoveWorktree(worktree.to_path_buf());
    let mut attempt = 1;
    loop {
        if !worktree.exists() {
            return Ok(());
        }
        match velocity_git::execute_git_command(repository, &command) {
            Ok(()) => return remove_leftover_folder(worktree),
            Err(error) if is_unregistered(&error) => return remove_leftover_folder(worktree),
            Err(source) if attempt >= REMOVE_ATTEMPTS => {
                return Err(DeletionError::Worktree {
                    path: display,
                    source: Box::new(source),
                });
            }
            Err(_) => {
                attempt += 1;
                std::thread::sleep(REMOVE_RETRY_DELAY);
            }
        }
    }
}

/// A thread whose setup failed early can leave a folder git never registered. It is still
/// the thread's own folder at its fixed worktree path, so it goes too: nothing may remain.
fn is_unregistered(error: &WorktreeError) -> bool {
    matches!(error, WorktreeError::CommandFailed { stderr, .. } if stderr.contains("is not a working tree"))
}

fn remove_leftover_folder(worktree: &Path) -> Result<(), DeletionError> {
    if !worktree.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(velocity_store::extended_windows_path(worktree)).map_err(|source| {
        DeletionError::Folder {
            path: worktree.display().to_string(),
            source,
        }
    })
}

/// Closes the thread's PR through `gh`. A PR that already merged or closed needs nothing.
pub fn close_pull_request(repository: &Path, number: u32) -> Result<(), DeletionError> {
    let closed = PullRequestClient::discover(repository)
        .and_then(|client| client.execute(PullRequestOperation::Close { number }));
    match closed {
        Ok(_) | Err(PullRequestError::Invalid(_)) => Ok(()),
        Err(source) => Err(DeletionError::PullRequest {
            number,
            source: Box::new(source),
        }),
    }
}
