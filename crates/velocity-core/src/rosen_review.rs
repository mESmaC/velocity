//! What a PR duty agent reviewing a Rosen change is told (F5, F7). The review is PR duty's,
//! with Rosen's own steps: the change's merge result is checked out by Rosen, a receipt
//! records the checks, and Rosen lands the change only with an approval at its head.

use crate::duty::PrDutyRuleSettings;
use crate::pr_review::{
    ReviewWork, SEVERITY_BAR, local_checks_step, quoted_list, short_change, short_commit,
};
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
    prompt.push_str(DECISION_RULES);
    if let Some(instruction) = brief.instruction {
        prompt.push_str(&format!("\nThe user asks PR duty: {instruction}\n"));
    }
    prompt.push_str("\nRun every command in the foreground and wait for it, however long it takes. Never end your turn before you record your decision: ending it stops your session, and a command left running in the background never calls you back.\nEnd your turn with a one-line summary of your decision.\n");
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
        return format!(
            "This folder is the change's merge result onto `{base}` ({}), written by Rosen: a plain build tree, not a git checkout and not the author's workspace. Your checks run here, on what would land. Nothing you edit here reaches the change.\n",
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
            "\nTier: sensitive. It touches {}, listed under Sensitive paths in AGENTS.md. Full review, plus:\n- Run the project's stress suite, the stress command AGENTS.md names as the merge gate, on the merge result before you land. A failure blocks the land.\n- Any proof-list feature it changes needs its confidence proof at the reference level (SPEC.md F17).\n{}",
            quoted_list(&brief.tier.sensitive_files),
            review_steps(brief.rules)
        ),
        ReviewTier::Normal | ReviewTier::Trivial => {
            format!(
                "\nTier: normal. Full review.\n{}",
                review_steps(brief.rules)
            )
        }
    }
}

fn review_steps(rules: &PrDutyRuleSettings) -> String {
    let mut steps = String::from(
        "Review:\n- Read AGENTS.md and SPEC.md first and follow AGENTS.md's rules for PR duty. Read the change with `rosen_duty_change`: its diff against its base, its named snapshots (the author's description of the work) and its review threads. A comment from a person is an instruction for this review: follow it. If one puts the change on hold, call `rosen_duty_review` with action `hold`, the head and the comment's reason, then stop.\n- Review the intent, not each line: check the change against its thread's task, issue or SPEC.md section, and AGENTS.md. Style is left to linters.\n",
    );
    if rules.check_diff_against_spec {
        steps.push_str("- Check the diff against SPEC.md: the main path of the claimed piece must do what its section says.\n");
    }
    steps.push_str(local_checks_step(rules));
    steps.push_str("- If `proofs/` changes, run `lake build` and the axiom audit. Never land unless the proofs check.\n");
    steps.push_str("- When you doubt a claim no proof covers, write a stress script, run it, and put its output in your review.\n");
    steps
}

const DECISION_RULES: &str = "\nDecide (locked rules):\n- Record your decision only with `rosen_duty_review`: action `approve`, `request_changes`, `comment` or `hold`, the head you checked and your reason. Requested changes go to the author's agent, which fixes them in its workspace and names the work again. An approval is Rosen's, bound to that head: a new head by the author voids it.\n- Before checking and again before landing, call `rosen_duty_prepare_landing`. It writes the change's current merge result into this folder, replacing what was there, and returns its head, base and conflicts. Run the checks there, then record them with `rosen_duty_checked` and that exact head and base: it submits the receipt Rosen needs to land.\n- After you approve, land with `rosen_duty_land` at the head you approved. If Rosen answers that the base moved and needs a new receipt, prepare, check and record again, then land. If it refuses for another reason, stop: Velocity records the failure.\n- Never approve, submit receipts or land with the `rosen` CLI, and never use git or `gh` for this change.\n";
