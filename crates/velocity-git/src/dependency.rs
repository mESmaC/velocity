//! Update a waiting thread without discarding local changes or rewriting published work.
use std::path::Path;

use crate::{GitError, command::git, short_checkout::CheckoutPath};

pub fn continue_after_dependency(
    repository: &Path,
    worktree: &Path,
    branch: &str,
    base: &str,
) -> Result<(), GitError> {
    crate::verify_thread_worktree(repository, worktree, branch).map_err(|error| {
        GitError::Failed {
            operation: "verify dependency checkout",
            path: worktree.into(),
            stderr: error.to_string(),
        }
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
    Ok(())
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
        stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
    })
}
