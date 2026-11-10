//! The files a worktree's own work touches (F6, F10): what a merged PR must overlap before
//! a swarm member is asked to rebase.
use std::collections::BTreeSet;
use std::path::Path;

use crate::command::git;
use crate::repository::GitError;

/// Committed and uncommitted changes since the worktree left `base`, plus untracked files,
/// as repository-relative paths with forward slashes.
pub fn worktree_changed_files(worktree: &Path, base: &str) -> Result<BTreeSet<String>, GitError> {
    let mut files = list(worktree, &["diff", "--name-only", "--merge-base", base])?;
    files.extend(list(
        worktree,
        &["ls-files", "--others", "--exclude-standard"],
    )?);
    Ok(files)
}

fn list(worktree: &Path, arguments: &[&str]) -> Result<BTreeSet<String>, GitError> {
    let output = git()
        .arg("-C")
        .arg(worktree)
        .args(["-c", "core.quotepath=false"])
        .args(arguments)
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        return Err(GitError::Failed {
            operation: "diff",
            path: worktree.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| line.replace('\\', "/"))
        .collect())
}
