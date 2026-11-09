//! Verify git's ownership at the I/O edge, then execute the existing proven
//! worktree lifecycle plan. This never prunes repository-wide metadata.
use std::path::{Path, PathBuf};
use std::process::Command;

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
    if !registered_worktree(repository, path, branch)? {
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
    let registered = registered_worktree(repository, path, branch)?;
    if path.exists() {
        verify_worktree(repository, path, branch)?;
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

fn registered_worktree(
    repository: &Path,
    path: &Path,
    branch: &str,
) -> Result<bool, MergedWorktreeError> {
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
            return Ok(true);
        }
        return Err(MergedWorktreeError::Ownership(
            "The registered worktree's branch changed; keeping it".into(),
        ));
    }
    if path.exists() {
        return Err(MergedWorktreeError::Ownership(
            "The path is not registered to this repository".into(),
        ));
    }
    // A crash after removal but before the checkpoint must be safe to retry.
    Ok(false)
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
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .arg("-c")
        .arg("core.longpaths=true")
        .args(arguments);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(MergedWorktreeError::Ownership(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().into())
}
