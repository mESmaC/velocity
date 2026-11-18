//! What a PR duty agent reviewing a Rosen change is told (F5, F7). The review is PR duty's,
//! with Rosen's own steps: the change's merge result is checked out by Rosen, a receipt
//! records the checks, and Rosen lands the change only with an approval at its head.
//!
//! Most reviews are by reading (F7 "Checks before merge"): the agent decides, and Velocity
//! runs the project's pre-land checks, submits the receipt and lands the approved head.

use crate::duty::PrDutyRuleSettings;
use crate::merge_gate::ReviewMode;
use crate::pr_review::{ReviewWork, SEVERITY_BAR, quoted_list, short_change, short_commit};
use crate::review_tier::{ReviewTier, TierAssessment};

/// Everything a Rosen review's prompt is built from.
#[derive(Clone, Copy, Debug)]
pub struct RosenReviewBrief<'a> {
    pub nickname: &'a str,
    pub change: &'a str,
    /// The thread's title, which names the work.
    pub title: &'a str,
    pub head: &'a str,
    pub base_branch: &'a str,
    pub base_commit: &'a str,
    /// Files that did not merge onto the base; empty when it merged.
    pub conflicts: &'a [String],
    pub work: ReviewWork,
    pub tier: &'a TierAssessment,
    /// Whether the agent reads the change or runs the local checks itself.
    pub mode: ReviewMode,
    pub rules: &'a PrDutyRuleSettings,
    pub instruction: Option<&'a str>,
}

/// The review's prompt. The locked rules are enforced by the `rosen_duty_*` tools and by
/// Rosen itself, which lands nothing without an approval at the head and a receipt.
pub fn rosen_review_prompt(brief: &RosenReviewBrief) -> String {
    let mut prompt = format!(
        "You are {}, on PR duty for this project (Velocity duty role: PR duty). You hold the review claim on Rosen change `{}` \"{}\": no other agent reviews, approves or lands it while you hold it. {}\n\n",
        brief.nickname,
        short_change(brief.change),
        brief.title,
        work_sentence(brief)
    );
    prompt.push_str(&folder_paragraph(brief));
    prompt.push_str(&tier_steps(brief));
    prompt.push_str(SEVERITY_BAR);
    prompt.push_str(match brief.mode {
        ReviewMode::Reading => READING_DECISION_RULES,
        ReviewMode::LocalChecks => DECISION_RULES,
    });
    if let Some(instruction) = brief.instruction {
        prompt.push_str(&format!("\nThe user asks PR duty: {instruction}\n"));
    }
    prompt.push_str(match brief.mode {
        ReviewMode::Reading => {
            "\nNever end your turn before you record your decision: ending it stops your session.\nEnd your turn with a one-line summary of your decision.\n"
        }
        ReviewMode::LocalChecks => {
            "\nQueue local checks that can take more than a minute with `queue_run` from `velocity-run`, using class `landing` in this merge result folder. End your turn after queuing and continue from Velocity's result. If the tool is unavailable, run the checks in the foreground.\nEnd the review with a one-line summary after recording your decision.\n"
        }
    });
    prompt
}

fn work_sentence(brief: &RosenReviewBrief) -> String {
    match brief.work {
        ReviewWork::Review => format!("Review its head {} now.", short_commit(brief.head)),
        ReviewWork::Merge => format!(
            "Its head {} is already approved in Rosen: run the local checks on the merge result, record them, and land it if they pass. If they fail, request changes with the failing output.",
            short_commit(brief.head)
        ),
    }
}

fn folder_paragraph(brief: &RosenReviewBrief) -> String {
    let base = brief.base_branch;
    if brief.conflicts.is_empty() {
        let used_for = match brief.mode {
            ReviewMode::Reading => {
                "It is here to read the change in context: build and run nothing in it."
            }
            ReviewMode::LocalChecks => "Your checks run here, on what would land.",
        };
        return format!(
            "This folder is the change's merge result onto `{base}` ({}), written by Rosen: a plain build tree, not a git checkout and not the author's workspace. {used_for} Nothing you edit here reaches the change.\n",
            short_commit(brief.base_commit)
        );
    }
    format!(
        "Rosen could not merge the change onto `{base}` ({}): it conflicts in {}. There is nothing to check yet. Request changes naming those files: the author resolves them in their workspace (`rosen resolve`) and names the work again.\n",
        short_commit(brief.base_commit),
        brief.conflicts.join(", ")
    )
}

/// F7 Tiered review. A Rosen change is tiered by its paths alone, so it is never trivial.
fn tier_steps(brief: &RosenReviewBrief) -> String {
    match brief.tier.tier {
        ReviewTier::Sensitive => format!(
            "\nTier: sensitive. It touches {}, listed under Sensitive paths in AGENTS.md. Full review, plus:\n- Run the project's designated stress suite on the merge result before you land. In the Velocity repository the command is `cargo run -p stress -- --all`. A failure blocks the land.\n- Any proof-list feature it changes needs its confidence proof at the reference level (SPEC.md F17).\n{}",
            quoted_list(&brief.tier.sensitive_files),
            review_steps(brief)
        ),
        ReviewTier::Normal | ReviewTier::Trivial => {
            format!("\nTier: normal. Full review.\n{}", review_steps(brief))
        }
    }
}

fn review_steps(brief: &RosenReviewBrief) -> String {
    let rules = brief.rules;
    let mut steps = String::from(
        "Review:\n- Start with the file list, diff stats, named snapshots and review threads from `rosen_duty_change`. A comment from a person is an instruction for this review: follow it. If one puts the change on hold, call `rosen_duty_review` with action `hold`, the head and the comment's reason, then stop.\n- Run `graph_impact` on changed paths indexed by the graph. Request patches with `rosen_duty_change` one path at a time and page through large files only as needed.\n- Use section references from the work or find the relevant headings with graph_find. Read them with graph_read (source: true), or read only their line ranges when the graph is unavailable.\n- Check the change against its thread's task, issue or SPEC.md section, and AGENTS.md. Style is left to linters.\n",
    );
    if rules.check_diff_against_spec {
        steps.push_str("- Check the diff against SPEC.md: the main path of the claimed piece must do what its section says.\n");
    }
    steps.push_str("- Compare proof scope and theorem statements to the project’s proof list and cited sections. Request changes for off-list Lean or single-case theorems. Implementation must outweigh proofs, tests and docs together.\n");
    if brief.mode == ReviewMode::Reading {
        steps.push_str("- Review by reading. Do not build, run the tests or run the local checks: once you approve, Velocity runs the project's pre-land checks (`rosen.toml`) on the merge result and lands the change only if they pass.\n");
        return steps;
    }
    // Velocity can't check this change itself: the agent's checks are the only ones it gets.
    if rules.ci_green_before_merge {
        steps.push_str("- Run the project's local checks from AGENTS.md here, on the merge result. Nothing else checks this change before it lands.\n");
    }
    steps.push_str("- If `proofs/` changes, run `lake build` and the axiom audit. Never land unless the proofs check.\n");
    steps.push_str("- When you doubt a claim no proof covers, write a stress script, run it, and put its output in your review.\n");
    steps
}

const READING_DECISION_RULES: &str = "\nDecide (locked rules):\n- Record your decision only with `rosen_duty_review`: action `approve`, `request_changes`, `comment` or `hold`, the head you read and your reason. Requested changes go to the author's agent, which fixes them in its workspace and names the work again. An approval is Rosen's, bound to that head: a new head by the author voids it.\n- Do not call `rosen_duty_prepare_landing`, `rosen_duty_checked` or `rosen_duty_land`. Velocity checks the head you approve on its merge result, submits the receipt and lands it. A failed check or a conflict sends the change back to its author with the output.\n- Never approve, submit receipts or land with the `rosen` CLI, and never use git or `gh` for this change.\n";

const DECISION_RULES: &str = "\nDecide (locked rules):\n- Record your decision only with `rosen_duty_review`: action `approve`, `request_changes`, `comment` or `hold`, the head you checked and your reason. Requested changes go to the author's agent, which fixes them in its workspace and names the work again. An approval is Rosen's, bound to that head: a new head by the author voids it.\n- Before checking and again before landing, call `rosen_duty_prepare_landing`. It writes the change's current merge result into this folder, replacing what was there, and returns its head, base and conflicts. Run the checks there, then record them with `rosen_duty_checked` and that exact head and base: it submits the receipt Rosen needs to land.\n- After you approve, land with `rosen_duty_land` at the head you approved. If Rosen answers that the base moved and needs a new receipt, prepare, check and record again, then land. If it refuses for another reason, stop: Velocity records the failure.\n- Never approve, submit receipts or land with the `rosen` CLI, and never use git or `gh` for this change.\n";
