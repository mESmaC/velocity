//! Verify git's ownership at the I/O edge, then execute the existing proven
//! worktree lifecycle plan. This never prunes repository-wide metadata.
use std::path::{Path, PathBuf};

use crate::{GitCommand, LifecycleEvent, WorktreeError, WorktreeRecord, execute_git_command};

#[derive(Debug, thiserror::Error)]
pub enum MergedWorktreeError {
    #[error("{0}")]
    Ownership(String),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Resuming must use the owning thread's existing linked checkout, never the base clone.
pub fn verify_thread_worktree(
    repository: &Path,
    path: &Path,
    branch: &str,
) -> Result<(), MergedWorktreeError> {
    if !repository.is_absolute() || !path.is_absolute() {
        return Err(MergedWorktreeError::Ownership(
            "Resuming requires absolute checkout paths".into(),
        ));
    }
    if path_identity(repository)? == path_identity(path)? {
        return Err(MergedWorktreeError::Ownership(
            "The project's checkout is not a thread worktree".into(),
        ));
    }
    if registered_worktree(repository, path, branch)? != Registration::Registered {
        return Err(MergedWorktreeError::Ownership(
            "The thread's worktree no longer exists".into(),
        ));
    }
    verify_worktree(repository, path, branch)
}

pub fn remove_merged_worktree(
    repository: &Path,
    path: &Path,
    branch: &str,
    delete_branch: bool,
) -> Result<(), MergedWorktreeError> {
    if !repository.is_absolute() || !path.is_absolute() {
        return Err(MergedWorktreeError::Ownership(
            "Worktree cleanup requires absolute Windows or Unix paths".into(),
        ));
    }
    // A project's checkout can itself be a linked worktree. Keep it even when
    // its branch and common git directory match the requested cleanup target.
    if path_identity(repository)? == path_identity(path)? {
        return Err(MergedWorktreeError::Ownership(
            "The project's checkout cannot be removed as a thread worktree".into(),
        ));
    }
    let registration = registered_worktree(repository, path, branch)?;
    let registered = registration == Registration::Registered;
    if registered && path.exists() {
        verify_deletion(repository, path, branch, true, false)?;
    }
    if registration == Registration::Leftover {
        remove_leftover_folder(path)?;
    }
    let mut lifecycle = WorktreeRecord::new(path, branch);
    for command in lifecycle.step(LifecycleEvent::PrMerged(delete_branch)) {
        match command {
            GitCommand::RemoveWorktree(path) if registered => {
                execute_git_command(repository, &GitCommand::RemoveWorktree(path.to_owned()))?;
            }
            GitCommand::DeleteBranch(branch)
                if git(
                    repository,
                    &["show-ref", "--verify", &format!("refs/heads/{branch}")],
                )
                .is_ok() =>
            {
                execute_git_command(repository, &GitCommand::DeleteBranch(branch.to_owned()))?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Removes a goal swarm agent's untouched choosing worktree and provisional branch when
/// nothing unclaimed remains in the pool (SPEC.md F5, F16). Fails closed if the path is
/// the project's checkout, has tracked edits, or has commits ahead of `base_ref`.
pub fn remove_untouched_worktree(
    repository: &Path,
    path: &Path,
    branch: &str,
    base_ref: &str,
) -> Result<(), MergedWorktreeError> {
    if !repository.is_absolute() || !path.is_absolute() {
        return Err(MergedWorktreeError::Ownership(
            "Worktree cleanup requires absolute Windows or Unix paths".into(),
        ));
    }
    if path_identity(repository)? == path_identity(path)? {
        return Err(MergedWorktreeError::Ownership(
            "The project's checkout cannot be removed as a thread worktree".into(),
        ));
    }
    let registration = registered_worktree(repository, path, branch)?;
    let registered = registration == Registration::Registered;
    if registration == Registration::Leftover {
        // An untouched choosing worktree holds no work; its leftover folder goes too.
        remove_leftover_folder(path)?;
    }
    if registered && path.exists() {
        verify_worktree(repository, path, branch)?;
        let status = git(path, &["status", "--porcelain", "--untracked-files=no"])?;
        if !status.is_empty() {
            return Err(MergedWorktreeError::Ownership(
                "The choosing worktree has tracked edits; keeping it".into(),
            ));
        }
        let ahead = git(path, &["rev-list", "--count", &format!("{base_ref}..HEAD")])?;
        if ahead != "0" {
            return Err(MergedWorktreeError::Ownership(
                "The choosing worktree has commits ahead of the base branch; keeping it".into(),
            ));
        }
    }
    if registered {
        execute_git_command(repository, &GitCommand::RemoveWorktree(path.to_path_buf()))?;
    }
    if git(
        repository,
        &["show-ref", "--verify", &format!("refs/heads/{branch}")],
    )
    .is_ok()
    {
        let _ = git(repository, &["branch", "-d", branch]);
    }
    Ok(())
}

/// What git and the disk say about a thread's worktree path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registration {
    /// Git lists the path as a linked worktree on the thread's branch.
    Registered,
    /// Nothing is at the path and git does not list it: already removed.
    Absent,
    /// An interrupted `git worktree remove` left the folder behind (see
    /// [`is_removal_leftover`]).
    Leftover,
}

fn registered_worktree(
    repository: &Path,
    path: &Path,
    branch: &str,
) -> Result<Registration, MergedWorktreeError> {
    let identity = path_identity(path)?;
    let worktrees = git(repository, &["worktree", "list", "--porcelain", "-z"])?;
    for record in worktrees.split("\0\0") {
        let mut fields = record.split('\0');
        let Some(directory) = fields
            .next()
            .and_then(|field| field.strip_prefix("worktree "))
        else {
            continue;
        };
        let Ok(registered_identity) = path_identity(Path::new(directory)) else {
            continue;
        };
        if registered_identity != identity {
            continue;
        }
        if fields.any(|field| field == format!("branch refs/heads/{branch}")) {
            return Ok(Registration::Registered);
        }
        return Err(MergedWorktreeError::Ownership(
            "The registered worktree's branch changed; keeping it".into(),
        ));
    }
    if path.exists() {
        if is_removal_leftover(repository, path) {
            return Ok(Registration::Leftover);
        }
        return Err(MergedWorktreeError::Ownership(
            "The path is not registered to this repository".into(),
        ));
    }
    // A crash after removal but before the checkpoint must be safe to retry.
    Ok(Registration::Absent)
}

/// What an interrupted `git worktree remove --force` leaves on Windows (#283, #293). Git
/// deletes the checkout's `.git` file first, stops at a file another process still holds
/// open, and drops the registration anyway. The folder is then no checkout: verification
/// would refuse it as unregistered for ever, so its thread's cleanup could never finish.
/// Nothing of git is left in it to lose (the branch and its commits live in the
/// repository), so callers remove the thread's own folder at its fixed path once the file
/// is released. Fails closed: any `.git` entry below it, or the repository inside it,
/// keeps it.
pub fn is_removal_leftover(repository: &Path, worktree: &Path) -> bool {
    // Canonical paths carry Windows' extended-length prefix, so the walk below reaches
    // folders past 260 characters.
    let (Ok(repository), Ok(worktree)) = (
        std::fs::canonicalize(repository),
        std::fs::canonicalize(worktree),
    ) else {
        return false;
    };
    !repository.starts_with(&worktree) && !contains_git_entry(&worktree)
}

/// Whether a `.git` file or folder is anywhere under `folder`, links not followed. An
/// unreadable folder counts as one.
fn contains_git_entry(folder: &Path) -> bool {
    let mut pending = Vec::from([folder.to_path_buf()]);
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
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
                pending.push(entry.path());
            }
        }
    }
    false
}

/// Fails while another process still holds a file in it; the next cleanup pass retries.
fn remove_leftover_folder(path: &Path) -> Result<(), MergedWorktreeError> {
    let canonical = std::fs::canonicalize(path)?;
    std::fs::remove_dir_all(canonical)?;
    Ok(())
}

fn path_identity(path: &Path) -> Result<PathBuf, MergedWorktreeError> {
    let mut parent = path;
    let mut suffix = Vec::new();
    while !parent.try_exists()? {
        suffix.push(parent.file_name().ok_or_else(|| {
            MergedWorktreeError::Ownership("The worktree drive is unavailable".into())
        })?);
        parent = parent.parent().ok_or_else(|| {
            MergedWorktreeError::Ownership("The worktree path has no absolute root".into())
        })?;
    }
    let mut resolved = std::fs::canonicalize(parent)?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    // Canonicalization uses the volume's actual spelling and resolves drive
    // aliases. Unicode case folding can merge names Windows keeps distinct.
    Ok(resolved)
}

/// A main checkout, another repository or another thread's branch fails closed.
fn verify_worktree(
    repository: &Path,
    path: &Path,
    branch: &str,
) -> Result<(), MergedWorktreeError> {
    if !path.join(".git").is_file() {
        return Err(MergedWorktreeError::Ownership(
            "The path is not a linked worktree".into(),
        ));
    }
    if git(path, &["branch", "--show-current"])? != branch {
        return Err(MergedWorktreeError::Ownership(
            "The worktree's branch changed; keeping it".into(),
        ));
    }
    if common_directory(repository)? != common_directory(path)? {
        return Err(MergedWorktreeError::Ownership(
            "The worktree belongs to another repository".into(),
        ));
    }
    Ok(())
}

fn common_directory(directory: &Path) -> Result<PathBuf, MergedWorktreeError> {
    Ok(std::fs::canonicalize(git(
        directory,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?)?)
}

fn git(directory: &Path, arguments: &[&str]) -> Result<String, MergedWorktreeError> {
    let mut command = crate::command::hidden_command("git");
    command
        .current_dir(directory)
        .arg("-c")
        .arg("core.longpaths=true")
        .args(arguments);
    let output = command.output()?;
    if !output.status.success() {
        return Err(MergedWorktreeError::Ownership(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}

/// Fail closed when git cannot establish ownership or remote reachability. Callers stop
/// the owning agent first so it cannot write between this check and removal.
pub fn verify_deletion(
    repository: &Path,
    path: &Path,
    branch: &str,
    merged: bool,
    thread_deleted: bool,
) -> Result<(), MergedWorktreeError> {
    verify_thread_worktree(repository, path, branch)?;
    // All remote refs matter: a pushed topic branch may already have been deleted
    // after a squash merge, while its commits remain reachable from another remote ref.
    let unpushed = !git(path, &["rev-list", "HEAD", "--not", "--remotes"])?.is_empty()
        || !git(path, &["status", "--porcelain", "--untracked-files=normal"])?.is_empty();
    if !crate::deletion_allowed(merged, thread_deleted, true, unpushed) {
        return Err(MergedWorktreeError::Ownership(
            "The worktree has uncommitted or unpushed work, or no deletion event; keeping it"
                .into(),
        ));
    }
    Ok(())
}
