//! Taking a worktree before the thread's title is known, then naming it from the title (F4).
//!
//! A thread's worktree is checked out under its message slug while the title model runs.
//! Once the title settles, the branch and folder are renamed to the title's slug and only
//! then is the setup step run. Renaming before setup matters on Windows: setup commands such
//! as `pnpm install` write junctions and caches with absolute paths that a later move would
//! break. The warm pool (M10) replaces only the checkout; this naming step stays.

use std::fs;
use std::path::{Path, PathBuf};

use crate::manager::{
    CreatedWorktree, WorktreeError, WorktreeSetupRequest, create_git_worktree_from_base,
    finish_worktree_setup, run_git_command, validate_worktree_path_length,
};

/// The names a worktree ends with once its title settles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeName {
    pub worktree_path: PathBuf,
    pub branch_name: String,
}

/// Where a titled worktree ended up, and how its setup went. The names are reported even
/// when setup fails: a failed thread keeps its worktree (F5) and a deleted one removes it
/// (F12), and both need the folder git actually left, not the one first planned.
#[derive(Debug)]
pub struct TitledWorktree {
    pub worktree_path: PathBuf,
    pub branch_name: Option<String>,
    pub setup: Result<CreatedWorktree, WorktreeError>,
}

/// Checks out `request` under its provisional names, waits for `final_name` (which blocks
/// until the title settles, at most F4's two seconds), renames, then runs the setup step.
///
/// `final_name` returning `None`, or a rename that fails, keeps the provisional names: F4
/// keeps the message slug whenever the title can't be used, and a thread must never fail
/// because of its title.
pub fn execute_titled_worktree_setup(
    request: &WorktreeSetupRequest,
    final_name: impl FnOnce() -> Option<WorktreeName>,
) -> TitledWorktree {
    let named = create_git_worktree_from_base(request, &request.worktree_path).map(|()| {
        match final_name() {
            Some(name) => rename_worktree(request, &name).unwrap_or_else(|_| request.clone()),
            None => request.clone(),
        }
    });
    match named {
        Ok(named) => TitledWorktree {
            setup: finish_worktree_setup(&named),
            worktree_path: named.worktree_path,
            branch_name: named.branch_name,
        },
        Err(error) => TitledWorktree {
            worktree_path: request.worktree_path.clone(),
            branch_name: request.branch_name.clone(),
            setup: Err(error),
        },
    }
}

/// Renames a worktree's branch in place while keeping its existing checkout folder (F16).
pub fn rename_worktree_branch(
    repository: &Path,
    old_branch: &str,
    new_branch: &str,
) -> Result<(), WorktreeError> {
    if old_branch != new_branch {
        run_git_command(repository, ["branch", "-m", old_branch, new_branch])?;
    }
    Ok(())
}

/// Renames the branch, then moves the folder, and returns the request under its new names.
/// A failed move renames the branch back, so the two never disagree.
pub fn rename_worktree(
    request: &WorktreeSetupRequest,
    name: &WorktreeName,
) -> Result<WorktreeSetupRequest, WorktreeError> {
    let mut renamed = request.clone();
    if let Some(old_branch) = &request.branch_name
        && *old_branch != name.branch_name
    {
        run_git_command(
            &request.repository_root,
            [
                "branch",
                "-m",
                old_branch.as_str(),
                name.branch_name.as_str(),
            ],
        )?;
        renamed.branch_name = Some(name.branch_name.clone());
    }
    if request.worktree_path != name.worktree_path {
        if let Err(error) = move_worktree(request, &name.worktree_path) {
            restore_branch(request, &renamed);
            return Err(error);
        }
        renamed.worktree_path = name.worktree_path.clone();
    }
    Ok(renamed)
}

fn move_worktree(request: &WorktreeSetupRequest, destination: &Path) -> Result<(), WorktreeError> {
    validate_worktree_path_length(destination)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| WorktreeError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let source = request.worktree_path.to_string_lossy().into_owned();
    let destination = destination.to_string_lossy().into_owned();
    run_git_command(
        &request.repository_root,
        ["worktree", "move", source.as_str(), destination.as_str()],
    )
}

fn restore_branch(original: &WorktreeSetupRequest, renamed: &WorktreeSetupRequest) {
    if let (Some(original_branch), Some(renamed_branch)) =
        (&original.branch_name, &renamed.branch_name)
        && original_branch != renamed_branch
    {
        // Best effort: if this fails too, the folder keeps the provisional name and the branch
        // the title's, which git still tracks correctly; only the names disagree.
        let _ = run_git_command(
            &original.repository_root,
            [
                "branch",
                "-m",
                renamed_branch.as_str(),
                original_branch.as_str(),
            ],
        );
    }
}
