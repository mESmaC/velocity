//! Proof checks before a PR duty review (SPEC.md F17 "What Velocity checks", M11).
//!
//! Every review of a new head runs them in the `review-pr-<n>` worktree before the agent sees
//! the PR, so they run again on every push: `lake build`, no `sorry` or `admit`, the axiom
//! audit and a named SPEC.md section, over the proof files the PR changed. The statement lock
//! is checked here too: when the head changes a theorem statement locked at the PR's last
//! approval, Velocity posts a `statement` event that withdraws the approval, and the review
//! restarts as a full review of every statement.

use std::path::Path;

use velocity_github::ReviewRecord;
use velocity_proofs::{check_worktree, load_statement_lock, statements_at};

/// The review being prepared: its project's approval record and the commits it checks.
pub(crate) struct ProofCheckTarget<'a> {
    pub app_data: &'a Path,
    pub project: &'a str,
    pub nickname: &'a str,
    pub number: u32,
    pub worktree: &'a Path,
    pub base_commit: &'a str,
    pub head: &'a str,
}

/// Runs the checks (blocking: Lake can take minutes) and returns what PR duty's prompt adds:
/// nothing when the PR touches no proofs and no locked statement moved.
pub(crate) fn check_before_review(target: &ProofCheckTarget) -> String {
    let mut sections = Vec::new();
    if let Some(reset) = statement_reset(target) {
        sections.push(reset);
    }
    match check_worktree(target.worktree, target.base_commit, target.head) {
        Ok(report) => sections.extend(report.summary()),
        Err(error) => sections.push(format!(
            "Proof checks could not run on this worktree ({error}). Run `lake build` in the proofs folder yourself before you decide."
        )),
    }
    sections
        .iter()
        .map(|section| format!("\n{section}\n"))
        .collect()
}

/// Compares the head's theorem statements with the lock taken at the last approval. When one
/// changed, posts the `statement` event and returns the prompt's restart notice.
fn statement_reset(target: &ProofCheckTarget) -> Option<String> {
    let lock = load_statement_lock(target.app_data, target.project, target.number).ok()??;
    if lock.head == target.head {
        return None;
    }
    let current = match statements_at(target.worktree, target.head) {
        Ok(current) => current,
        Err(error) => {
            return Some(format!(
                "Velocity could not compare this head's theorem statements with those locked at the last approval ({error}). Review every theorem statement afresh."
            ));
        }
    };
    let changed = lock.changed_in(&current);
    if changed.is_empty() {
        return None;
    }
    // Best effort: the prompt still restarts the review if the event can't be posted.
    let _ =
        ReviewRecord::open(target.app_data, target.project, target.nickname).and_then(|record| {
            record.withdraw_for_statements(target.number, target.head, &lock.head, &changed)
        });
    Some(format!(
        "Statement lock: this head changes or removes theorem statements locked when #{} was approved at {}: {}. Velocity withdrew that approval (a `statement` event). This review restarts from the beginning: it is not a later round, so review every theorem statement against its SPEC.md section, and request changes on any statement weakened to make its proof go through.",
        target.number,
        lock.head.get(..7).unwrap_or(&lock.head),
        changed.join(", ")
    ))
}
