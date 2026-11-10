//! PR duty review worktrees (SPEC.md F5 path scheme `review-pr-<n>`, F7 PR duty): a detached
//! checkout of the PR's head with the base branch merged in, so the review's local checks run
//! on the merge result rather than the old PR head. Nothing here is pushed; the reviewing
//! agent decides whether to update the PR branch.

use std::path::{Path, PathBuf};

use velocity_core::review_diff::summarize_diff;
use velocity_core::review_tier::{TierAssessment, assess_tier, sensitive_paths};

use crate::command::git;
use crate::manager::{WorktreeError, WorktreeSetupRequest, run_git_command};
use crate::sweep::{prepare_sweep_worktree, resolve_commit};

/// A prepared review checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewCheckout {
    pub path: PathBuf,
    /// The PR head the checkout was made from: the head an approval binds to.
    pub head: String,
    /// The base commit merged onto it.
    pub base_commit: String,
    /// Files that conflict when the base is merged in. When this is not empty the checkout is
    /// left at the PR head alone, and the agent resolves or sends the conflict back.
    pub conflicts: Vec<String>,
}

/// Makes or resets the `review-pr-<n>` worktree (`request.base_ref` is the base commit), checks
/// out PR `number`'s head, and merges the base into it. A folder at that path that is not one of
/// this repository's worktrees is never touched.
pub fn prepare_review_worktree(
    request: &WorktreeSetupRequest,
    number: u32,
) -> Result<ReviewCheckout, WorktreeError> {
    let path = prepare_sweep_worktree(request)?;
    // Fetched inside the review worktree: FETCH_HEAD is per worktree, so reviews running at
    // the same time never read each other's fetch.
    run_git_command(
        &path,
        [
            "fetch",
            "--quiet",
            "origin",
            &format!("+refs/pull/{number}/head"),
        ],
    )?;
    let head = resolve_commit(&path, "FETCH_HEAD").ok_or_else(|| failed(&path, "FETCH_HEAD"))?;
    run_git_command(&path, ["checkout", "--quiet", "--detach", "--force", &head])?;
    // Leftovers from an earlier review of this PR would be checked as part of this one.
    run_git_command(&path, ["clean", "--quiet", "-d", "--force"])?;
    let base_commit = request.base_ref.clone();
    let conflicts = merge_base(&path, &base_commit)?;
    Ok(ReviewCheckout {
        path,
        head,
        base_commit,
        conflicts,
    })
}

/// Merges `base_commit` into the checkout. Returns the conflicting files, after aborting the
/// merge, or nothing when it merged (or was already contained).
fn merge_base(path: &Path, base_commit: &str) -> Result<Vec<String>, WorktreeError> {
    let message = format!("Merge {} into the PR for review", short(base_commit));
    let merged = run_git_command(
        path,
        ["merge", "--quiet", "--no-ff", "-m", &message, base_commit],
    );
    let Err(error) = merged else {
        return Ok(Vec::new());
    };
    let conflicts = unmerged_files(path);
    // Aborting is best effort: a merge that never started has nothing to abort.
    let _ = run_git_command(path, ["merge", "--abort"]);
    if conflicts.is_empty() {
        return Err(error);
    }
    Ok(conflicts)
}

fn unmerged_files(path: &Path) -> Vec<String> {
    let Ok(output) = git()
        .current_dir(path)
        .args(["diff", "--name-only", "--diff-filter=U"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The review's tier (F7 Tiered review): the PR's own changes (head against its merge base
/// with the base commit) checked against the Sensitive paths of the base commit's AGENTS.md.
/// The base's AGENTS.md, not the PR's, so a PR cannot lower its own tier by editing the list.
pub fn review_tier(
    worktree: &Path,
    base_commit: &str,
    head: &str,
) -> Result<TierAssessment, WorktreeError> {
    let range = format!("{base_commit}...{head}");
    let diff = git_output(
        worktree,
        &[
            "-c",
            "core.quotepath=false",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--find-renames",
            &range,
        ],
    )?;
    let agents = git_output(worktree, &["show", &format!("{base_commit}:AGENTS.md")])
        // A base without AGENTS.md lists no sensitive paths.
        .unwrap_or_default();
    Ok(assess_tier(
        &summarize_diff(&diff),
        &sensitive_paths(&agents),
    ))
}

fn git_output(path: &Path, arguments: &[&str]) -> Result<String, WorktreeError> {
    let output = git()
        .current_dir(path)
        .args(arguments)
        .output()
        .map_err(|error| WorktreeError::CommandFailed {
            command: format!("git {}", arguments.join(" ")),
            working_directory: path.to_path_buf(),
            status: "not started".into(),
            stderr: error.to_string(),
        })?;
    if !output.status.success() {
        return Err(WorktreeError::CommandFailed {
            command: format!("git {}", arguments.join(" ")),
            working_directory: path.to_path_buf(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Removes a review worktree once its PR merged (Rule 5). Never removes the project checkout.
pub fn remove_review_worktree(repository_root: &Path, path: &Path) -> Result<(), WorktreeError> {
    let same = std::fs::canonicalize(repository_root).ok() == std::fs::canonicalize(path).ok();
    if same || !path.exists() {
        return Ok(());
    }
    crate::manager::execute_git_command(
        repository_root,
        &crate::GitCommand::RemoveWorktree(path.to_path_buf()),
    )
}

fn short(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}

fn failed(path: &Path, reference: &str) -> WorktreeError {
    WorktreeError::CommandFailed {
        command: format!("git rev-parse {reference}"),
        working_directory: path.to_path_buf(),
        status: "missing".into(),
        stderr: format!("{reference} does not name a commit after the fetch"),
    }
}
