//! F17 landing gate: executable checks apply to the exact committed merge result.
use std::path::Path;
use std::process::Command;

use crate::PullRequestError;

/// Re-runs proof checks, refusing dirty or stale review checkouts. The reviewer still judges
/// proof-list coverage and theorem meaning; a successful Lake build cannot establish those.
pub fn check_proof_gate(worktree: &Path, base: &str, head: &str) -> Result<(), PullRequestError> {
    verify_checkout(worktree, head)?;
    let report = velocity_proofs::check_worktree(worktree, base, head)
        .map_err(|error| PullRequestError::Invalid(format!("Proof gate could not run: {error}")))?;
    if !report.passed() {
        return Err(PullRequestError::Invalid(
            report.summary().unwrap_or_default(),
        ));
    }
    if let velocity_proofs::CheckStep::Ran(audit) = &report.axioms
        && !audit.unbuilt.is_empty()
    {
        return Err(PullRequestError::Invalid(format!(
            "Proof gate: lake build did not check these changed modules: {}",
            audit.unbuilt.join(", ")
        )));
    }
    verify_checkout(worktree, head)
}

fn verify_checkout(worktree: &Path, head: &str) -> Result<(), PullRequestError> {
    if git(worktree, &["rev-parse", "HEAD"])?.trim() != head {
        return Err(PullRequestError::Invalid(
            "Proof gate: check out the prepared head in the review worktree first".into(),
        ));
    }
    // Untracked Lean can shadow an import too; ignored Lake build products are excluded.
    if !git(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )?
    .trim()
    .is_empty()
    {
        return Err(PullRequestError::Invalid(
            "Proof gate: commit or remove review worktree changes before checking".into(),
        ));
    }
    Ok(())
}

fn git(worktree: &Path, arguments: &[&str]) -> Result<String, PullRequestError> {
    let mut command = Command::new("git");
    command
        .args(["-c", "core.longpaths=true"])
        .args(arguments)
        .current_dir(worktree);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(PullRequestError::Invalid(format!(
            "Proof gate git failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
