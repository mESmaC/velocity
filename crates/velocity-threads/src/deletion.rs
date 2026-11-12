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
    #[error(transparent)]
    Guard(#[from] velocity_git::MergedWorktreeError),
    #[error("Could not close PR #{number}: {source}")]
    PullRequest {
        number: u32,
        #[source]
        source: Box<PullRequestError>,
    },
}

/// What one Delete takes: a thread, or a swarm or goal swarm with its members (F10, F12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deletion {
    Thread(ThreadKey),
    Swarm {
        project: u32,
        swarm: u32,
        /// The members when Delete was chosen; landing reads them again.
        members: Vec<u32>,
    },
}

impl Deletion {
    pub fn project(&self) -> u32 {
        match self {
            Self::Thread((project, _)) | Self::Swarm { project, .. } => *project,
        }
    }

    /// The sidebar rows hidden while Undo is on offer.
    pub fn rows(&self) -> Vec<usize> {
        match self {
            Self::Thread((_, thread)) => Vec::from([*thread as usize]),
            Self::Swarm { swarm, members, .. } => std::iter::once(swarm)
                .chain(members)
                .map(|row| *row as usize)
                .collect(),
        }
    }

    /// Whether this deletion takes the thread or swarm `key` names.
    pub fn covers(&self, key: ThreadKey) -> bool {
        self.project() == key.0 && self.rows().contains(&(key.1 as usize))
    }
}

/// Deletions inside their undo window. Undo and landing each take the deletion out, so
/// whichever comes first wins and the other finds nothing.
#[derive(Debug, Default)]
pub struct PendingDeletions {
    next: u32,
    pending: BTreeMap<u32, Deletion>,
}

impl PendingDeletions {
    /// Starts the undo window for a deletion and returns its token.
    pub fn begin(&mut self, deletion: Deletion) -> u32 {
        self.next = self.next.wrapping_add(1);
        self.pending.insert(self.next, deletion);
        self.next
    }

    /// Whether a pending deletion takes this thread, swarm or swarm member.
    pub fn is_pending(&self, key: ThreadKey) -> bool {
        self.pending.values().any(|pending| pending.covers(key))
    }

    /// Undo: everything comes back untouched.
    pub fn undo(&mut self, token: u32) -> Option<Deletion> {
        self.pending.remove(&token)
    }

    /// The window is over: the deletion lands unless it was undone.
    pub fn land(&mut self, token: u32) -> Option<Deletion> {
        self.pending.remove(&token)
    }
}

/// What landing a deletion does besides stopping the agent and dropping the record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletionPlan {
    pub repository: PathBuf,
    pub branch: String,
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
        branch: thread.branch_name.clone(),
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

/// Verify the saved owning branch before explicit thread deletion.
pub fn remove_owned_worktree(
    repository: &Path,
    worktree: &Path,
    branch: &str,
) -> Result<(), DeletionError> {
    if worktree.exists() && !is_removal_leftover(repository, worktree) {
        velocity_git::verify_deletion(repository, worktree, branch, false, true)?;
    }
    remove_worktree(repository, worktree)
}

/// What an interrupted `git worktree remove --force` leaves on Windows (#283). Git deletes
/// the checkout's `.git` file first, stops at a file another process still holds open,
/// and drops the registration anyway. The folder is then no checkout: verification would
/// refuse it as unregistered for ever, so the thread could never be deleted. Nothing of git
/// is left in it to lose (the branch and its commits live in the repository), so the
/// thread's own folder at its fixed path is removed once the file is released.
/// Fails closed: any `.git` entry below it, or the repository inside it, keeps it.
fn is_removal_leftover(repository: &Path, worktree: &Path) -> bool {
    let repository_inside = match (
        std::fs::canonicalize(repository),
        std::fs::canonicalize(worktree),
    ) {
        (Ok(repository), Ok(worktree)) => repository.starts_with(worktree),
        _ => true,
    };
    !repository_inside && !contains_git_entry(worktree)
}

/// Whether a `.git` file or folder is anywhere under `folder`, links not followed. An
/// unreadable folder counts as one.
fn contains_git_entry(folder: &Path) -> bool {
    let mut pending = Vec::from([folder.to_path_buf()]);
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(velocity_store::extended_windows_path(&directory))
        else {
            return true;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return true;
            };
            if entry.file_name() == ".git" {
                return true;
            }
            let Ok(kind) = entry.file_type() else {
                return true;
            };
            if kind.is_dir() && !kind.is_symlink() {
                pending.push(directory.join(entry.file_name()));
            }
        }
    }
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    // Risk: deleting a thread removes the project's own checkout. A spec thread runs in the
    // main checkout (F3), and a recorded worktree path can resolve to the repository itself
    // (the same folder spelled differently); neither may be removed.
    #[test]
    fn deletion_never_removes_the_main_checkout() -> Result<(), std::io::Error> {
        let repository = std::env::temp_dir().join(format!("velocity-f12-{}", std::process::id()));
        std::fs::create_dir_all(&repository)?;
        let spec_path = repository.display().to_string();
        let spec_owns_it = owns_worktree(ThreadState::Spec.into(), Some(&spec_path));
        let refused = remove_worktree(&repository, &repository.join("."));
        let still_there = repository.is_dir();
        std::fs::remove_dir_all(&repository)?;
        assert!(!spec_owns_it);
        assert!(matches!(refused, Err(DeletionError::MainCheckout(_))));
        assert!(still_there);
        Ok(())
    }

    // Risk (#283): a `git worktree remove --force` interrupted by a file another process
    // holds open deletes the checkout's `.git` file and its registration but leaves a
    // folder; verification then refused it for ever, so the thread could never be deleted.
    // The leftover must go, but a folder still holding any `.git` entry must not.
    #[test]
    fn interrupted_removal_leftover_goes_but_a_checkout_inside_stays()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("velocity-283-{}", std::process::id()));
        let repository = root.join("repository");
        let worktree = root.join("worktrees").join("leftover");
        std::fs::create_dir_all(&repository)?;
        let git = |arguments: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
            let status = std::process::Command::new("git")
                .current_dir(&repository)
                .args([
                    "-c",
                    "user.name=velocity",
                    "-c",
                    "user.email=velocity@example.com",
                ])
                .args(arguments)
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err("git failed".into())
            }
        };
        git(&["init", "-q"])?;
        git(&["commit", "-q", "--allow-empty", "-m", "base"])?;
        let path = worktree.display().to_string();
        git(&["worktree", "add", "-q", "-b", "vel/leftover", &path])?;
        // What git leaves: no `.git` file, no registration, the rest of the folder.
        std::fs::remove_file(worktree.join(".git"))?;
        std::fs::remove_dir_all(repository.join(".git").join("worktrees"))?;
        std::fs::create_dir_all(worktree.join("held").join(".git"))?;
        let kept = remove_owned_worktree(&repository, &worktree, "vel/leftover");
        let kept_folder = worktree.is_dir();
        std::fs::remove_dir_all(worktree.join("held"))?;
        let removed = remove_owned_worktree(&repository, &worktree, "vel/leftover");
        let removed_folder = !worktree.exists();
        std::fs::remove_dir_all(&root)?;
        assert!(kept.is_err() && kept_folder);
        assert!(removed.is_ok() && removed_folder);
        Ok(())
    }
}
