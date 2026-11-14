//! Velocity's checker for Rosen changes (SPEC.md F7 "Checks before merge"; Rosen §9 "Build
//! queue"): which changes PR duty reviews by reading, what a refused land asks for next, and
//! what the author and the Duty feed are told. Pure decisions; the Rosen calls live in
//! `velocity-rosen` and the pass that drives them in `velocity-threads`.

use crate::merge_gate::{ModeEvidence, ReviewMode, review_mode};
use crate::pr_review::{short_change, short_commit};
use crate::review_tier::ReviewTier;
use crate::scout_sweep::step_doing;

/// The folder a project's Lake project lives in (F17).
const PROOFS_FOLDER: &str = "proofs/";
/// The most of a failed command's output its author is sent: the end, where the error is.
const OUTPUT_TAIL: usize = 4000;

/// A Rosen change is reviewed by reading when Velocity can check it instead: the project's
/// `rosen.toml` names pre-land commands, and nothing only an agent's run covers is at stake
/// (a sensitive path's stress gate, a changed proof).
pub fn change_review_mode(
    tier: ReviewTier,
    paths: &[String],
    pre_land_commands: bool,
) -> ReviewMode {
    review_mode(ModeEvidence {
        tier,
        // Rosen lands nothing without a receipt, whatever the role's rule says.
        checks_required: true,
        has_github_checks: pre_land_commands,
        changes_proofs: paths.iter().any(|path| path.starts_with(PROOFS_FOLDER)),
    })
}

/// What a land Rosen refused needs before it can land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LandRefusal {
    /// A receipt for the current merge result: none exists, or the base moved under it.
    Receipt,
    /// The change no longer merges onto the base: its author resolves it.
    Conflict,
    /// The approval no longer stands: the author moved the head, so it is reviewed again.
    Approval,
    /// Anything else, such as a parent change that has not landed yet.
    Other,
}

/// Reads Rosen's stable refusal code.
pub fn land_refusal(code: &str) -> LandRefusal {
    match code {
        "no_receipt" | "receipt_stale" | "needs_recheck" => LandRefusal::Receipt,
        "merge_conflict" => LandRefusal::Conflict,
        "unapproved" => LandRefusal::Approval,
        _ => LandRefusal::Other,
    }
}

/// A pre-land command that did not pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedCheck {
    pub command: String,
    /// `None` when the command was stopped for running too long.
    pub exit_code: Option<i32>,
    /// What it printed, standard output and error together.
    pub output: String,
}

/// What the author's agent is told when a pre-land check fails on its approved change.
pub fn check_failed_message(change: &str, base: &str, failed: &FailedCheck) -> String {
    let ended = match failed.exit_code {
        Some(code) => format!("exited with status {code}"),
        None => "ran too long and was stopped".to_owned(),
    };
    format!(
        "Velocity ran the project's pre-land checks (rosen.toml) on your approved Rosen change `{}`, merged onto the base at {}. `{}` {ended}. The end of its output:\n\n{}\n\nFix it in your workspace. If your workspace is behind the base, apply the pending rebase first (`rosen rebase`), since the failure may come from the merge. Then name the work again with `rosen commit -m \"<what changed>\"` so PR duty reviews it again. If the failure is the machine's and not your change's (a missing tool, a full disk), say so in your thread instead of changing code.\n",
        short_change(change),
        short_commit(base),
        failed.command,
        tail(&failed.output),
    )
}

/// What the author's agent is told when its approved change no longer merges onto the base.
pub fn conflict_message(change: &str, base_branch: &str, files: &[String]) -> String {
    format!(
        "Your approved Rosen change `{}` no longer merges onto `{base_branch}`: it conflicts in {}. Run `rosen rebase` in your workspace, combine both sides between the conflict markers, run `rosen resolve <file>` for each file, then name the work again with `rosen commit -m \"<what changed>\"` so PR duty reviews it again.\n",
        short_change(change),
        files.join(", "),
    )
}

/// The feed line while the checks run.
pub fn checking_line(change: &str, base: &str) -> String {
    format!(
        "Checking change {} on {}",
        short_change(change),
        short_commit(base)
    )
}

/// The feed line for a change sent back over a failed check.
pub fn check_failed_line(change: &str, command: &str) -> String {
    step_doing(&format!(
        "Checks failed on change {} · `{command}` · sent back to its author",
        short_change(change)
    ))
}

/// The feed line for a change sent back over a conflict.
pub fn conflict_line(change: &str, base_branch: &str) -> String {
    format!(
        "Change {} conflicts with `{base_branch}` · sent back to its author",
        short_change(change)
    )
}

/// The feed line for a landed change. `checked` is false when Rosen needed no new receipt:
/// a prose-only change, or one whose receipt still counted.
pub fn landed_line(change: &str, commit: &str, checked: bool) -> String {
    let how = if checked {
        "pre-land checks passed"
    } else {
        "no new check needed"
    };
    format!(
        "Landed change {} · {} · {how}",
        short_change(change),
        short_commit(commit)
    )
}

/// The feed line for a land Rosen refused for a reason no check or author fixes.
pub fn refused_line(change: &str, reason: &str) -> String {
    step_doing(&format!(
        "Land of change {} refused · {reason}",
        short_change(change)
    ))
}

/// The last `OUTPUT_TAIL` characters of `output`, from the start of a line.
fn tail(output: &str) -> &str {
    let output = output.trim_end();
    if output.len() <= OUTPUT_TAIL {
        return output;
    }
    let mut start = output.len() - OUTPUT_TAIL;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    match output[start..].find('\n') {
        Some(line_end) => &output[start + line_end + 1..],
        None => &output[start..],
    }
}
