//! Update a waiting thread without discarding local changes or rewriting published work.
use std::path::Path;

use crate::{GitError, command::git, short_checkout::CheckoutPath};

/// Merges `base` into the branch checked out in the thread's registered worktree and
/// returns that branch, which may differ from `registered_branch` once the thread has
/// moved on to a new branch for its next piece (#347).
pub fn continue_after_dependency(
    repository: &Path,
    worktree: &Path,
    registered_branch: &str,
    base: &str,
) -> Result<String, GitError> {
    let branch =
        crate::merged_worktree::thread_worktree_branch(repository, worktree, registered_branch)
            .map_err(|error| GitError::Failed {
                operation: "verify dependency checkout",
                path: worktree.into(),
                stderr: error.to_string(),
            })?;
    let checkout = CheckoutPath::new(worktree).map_err(GitError::Spawn)?;
    let directory = checkout.path();
    run(
        directory,
        &["check-ref-format", &format!("refs/heads/{base}")],
    )?;
    run(
        directory,
        &["fetch", "origin", &format!("refs/heads/{base}")],
    )?;
    // Published branch history belongs to PR duty too. A merge is the repository's
    // non-rewriting equivalent of rebasing; conflicts and dirty changes stay recoverable.
    run(directory, &["merge", "--no-edit", "FETCH_HEAD"])?;
    Ok(branch)
}

fn run(directory: &Path, arguments: &[&str]) -> Result<(), GitError> {
    let output = git()
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .map_err(GitError::Spawn)?;
    if output.status.success() {
        return Ok(());
    }
    Err(GitError::Failed {
        operation: "continue dependency",
        path: directory.into(),
        stderr: format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .trim()
        .into(),
    })
}
