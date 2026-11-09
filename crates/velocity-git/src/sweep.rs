//! Scout sweep worktrees (SPEC.md F5 path scheme `<scout>-sweep`, F7 Scouts): a detached
//! checkout of the base branch's tip, made on a scout's first sweep and reset before each
//! later one, so every sweep looks at what is on the base branch now.

use std::path::{Path, PathBuf};

use crate::command::git;
use crate::manager::{
    WorktreeError, WorktreeSetupRequest, execute_worktree_setup, run_git_command,
};

/// The commit a sweep starts from: the remote's base branch when it can be fetched, so
/// "after every merge" sees merges made on GitHub, otherwise the local branch.
pub fn sweep_base(repository_root: &Path, base_branch: &str) -> Option<(String, String)> {
    let remote_ref = format!("refs/remotes/origin/{base_branch}");
    // Offline or no remote: the local branch is still a valid base.
    let fetched = run_git_command(repository_root, ["fetch", "--quiet", "origin", base_branch]);
    let candidates = if fetched.is_ok() {
        vec![remote_ref, base_branch.to_owned()]
    } else {
        vec![base_branch.to_owned(), remote_ref]
    };
    candidates.into_iter().find_map(|reference| {
        resolve_commit(repository_root, &reference).map(|commit| (reference, commit))
    })
}

/// The commit `reference` names, if it exists.
pub fn resolve_commit(repository_root: &Path, reference: &str) -> Option<String> {
    let output = git()
        .current_dir(repository_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{reference}^{{commit}}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!commit.is_empty()).then_some(commit)
}

/// Makes the sweep worktree at `request.base_ref`, or resets the scout's existing one there.
/// A folder at that path that is not one of this repository's worktrees is never touched.
pub fn prepare_sweep_worktree(request: &WorktreeSetupRequest) -> Result<PathBuf, WorktreeError> {
    let path = &request.worktree_path;
    if !path.exists() {
        return execute_worktree_setup(request).map(|created| created.worktree_path);
    }
    if !is_registered_worktree(&request.repository_root, path)? {
        return Err(WorktreeError::CommandFailed {
            command: "git worktree list --porcelain".into(),
            working_directory: request.repository_root.clone(),
            status: "not a worktree".into(),
            stderr: format!(
                "{} exists but is not a worktree of this repository; move it and sweep again",
                path.display()
            ),
        });
    }
    // The checkout is scratch space: scouts file what they find instead of committing it.
    run_git_command(
        path,
        [
            "checkout",
            "--quiet",
            "--detach",
            "--force",
            &request.base_ref,
        ],
    )?;
    Ok(path.clone())
}

fn is_registered_worktree(repository_root: &Path, path: &Path) -> Result<bool, WorktreeError> {
    let output = git()
        .current_dir(repository_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|source| WorktreeError::CommandFailed {
            command: "git worktree list --porcelain".into(),
            working_directory: repository_root.to_path_buf(),
            status: "could not start".into(),
            stderr: source.to_string(),
        })?;
    let wanted = std::fs::canonicalize(path).ok();
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|listed| wanted.is_some() && std::fs::canonicalize(listed).ok() == wanted))
}
