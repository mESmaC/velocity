//! PR duty reviews (SPEC.md F7 PR duty, Rule 4): which open PRs need an agent, which agent
//! takes which PR (one PR per agent, so two agents never review the same one), what the
//! reviewing agent is told, and what its card and the Duty view's activity feed say.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::duty::{DutyMergeMethod, PrDutyRuleSettings};
use crate::review_tier::{ReviewTier, TierAssessment};
use crate::scout_sweep::step_doing;

/// Where Velocity's approval record (F7, Q28) leaves a PR's current head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewStanding {
    /// No decision covers this head: it needs a review.
    Unreviewed,
    /// The standing approval is at exactly this head: it can merge.
    Approved,
    /// Changes were requested at this head: the author has to push.
    ChangesRequested,
    /// The approved head failed to merge (e.g. branch protection): it waits for a new head.
    MergeFailed,
}

/// An open PR as PR duty sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewCandidate {
    pub number: u32,
    pub title: String,
    pub head: String,
    pub head_branch: String,
    pub draft: bool,
    pub standing: ReviewStanding,
}

/// What an agent does with the PR it claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewWork {
    /// Review the head, then approve and merge, or request changes.
    Review,
    /// The head is already approved: re-run the checks on the merge result and merge.
    Merge,
}

/// How many times a review that ends a turn without a decision is asked to carry on before
/// its session stops. Bounds a broken agent; a working review decides well before it.
pub const UNDECIDED_NUDGES: u32 = 3;

/// How long a head whose review ended without a decision rests before it is claimed again.
/// Long enough that a broken agent can't loop on it, short enough that a ready PR never
/// waits on the user (Rule 1: no "needs you" state).
pub const UNDECIDED_RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// What happens when a review's agent ends a turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnEndAction {
    /// An instruction from the composer is queued: its turn comes next.
    AnswerInstruction,
    /// No decision yet: ask the agent to carry on (`continue_prompt`).
    Nudge,
    /// The review is over: it decided, or ran out of nudges.
    Stop,
}

/// A review lasts until its agent records a decision, not one turn: an agent that ends its
/// turn to wait for a long command would otherwise be cut off mid-review (#237).
pub fn turn_end_action(
    pending_instructions: usize,
    decided: bool,
    nudges_sent: u32,
) -> TurnEndAction {
    if pending_instructions > 0 {
        TurnEndAction::AnswerInstruction
    } else if decided || nudges_sent >= UNDECIDED_NUDGES {
        TurnEndAction::Stop
    } else {
        TurnEndAction::Nudge
    }
}

/// The message that asks an agent which ended its turn undecided to carry on.
pub fn continue_prompt(number: u32, nudge: u32) -> String {
    format!(
        "You ended your turn without recording a decision on PR #{number} (reminder {nudge} of {UNDECIDED_NUDGES}). Your session stops when a turn ends without one, so nothing you left running in the background will call you back. Continue the review now: run any long command in the foreground and wait for it, then record your decision with `pr_duty_review` (and merge with `pr_duty_merge` once approved). If you cannot decide, say why in one line.\n"
    )
}

/// The last session on a PR, as the runner remembers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewAttempt {
    /// The head the session reviewed.
    pub head: String,
    /// Whether it recorded a decision (approve, request changes, merge or a refused merge).
    pub decided: bool,
    /// When it ended, on the runner's clock.
    pub ended_at: Duration,
}

/// The heads not to claim now: a decided head until it moves (its standing says what it
/// needs next), an undecided one only until `UNDECIDED_RETRY_AFTER` has passed, so one bad
/// session never strands a PR.
pub fn resting_heads(
    attempts: &BTreeMap<u32, ReviewAttempt>,
    now: Duration,
) -> BTreeMap<u32, String> {
    attempts
        .iter()
        .filter(|(_, attempt)| {
            attempt.decided || now.saturating_sub(attempt.ended_at) < UNDECIDED_RETRY_AFTER
        })
        .map(|(number, attempt)| (*number, attempt.head.clone()))
        .collect()
}

/// The work a PR needs now, if any. A draft is still a claim, not a PR to review; a resting
/// head (`resting_heads`) is not claimed until it moves or its rest ends.
pub fn review_work(
    candidate: &ReviewCandidate,
    attempted_head: Option<&str>,
) -> Option<ReviewWork> {
    if candidate.draft || attempted_head == Some(candidate.head.as_str()) {
        return None;
    }
    match candidate.standing {
        ReviewStanding::Unreviewed => Some(ReviewWork::Review),
        ReviewStanding::Approved => Some(ReviewWork::Merge),
        ReviewStanding::ChangesRequested | ReviewStanding::MergeFailed => None,
    }
}

/// One agent's next claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewAssignment {
    pub agent: String,
    pub number: u32,
    pub work: ReviewWork,
}

/// Pairs idle agents with PRs that need work, oldest PR first. A PR another agent holds is
/// never handed out, and each agent gets at most one PR (F7: one PR per agent).
pub fn assign_reviews(
    candidates: &[ReviewCandidate],
    idle_agents: &[String],
    held: &BTreeSet<u32>,
    attempted: &BTreeMap<u32, String>,
) -> Vec<ReviewAssignment> {
    let mut open: Vec<(u32, ReviewWork)> = candidates
        .iter()
        .filter(|candidate| !held.contains(&candidate.number))
        .filter_map(|candidate| {
            let tried = attempted.get(&candidate.number).map(String::as_str);
            review_work(candidate, tried).map(|work| (candidate.number, work))
        })
        .collect();
    open.sort_by_key(|(number, _)| *number);
    open.dedup_by_key(|(number, _)| *number);
    idle_agents
        .iter()
        .zip(open)
        .map(|(agent, (number, work))| ReviewAssignment {
            agent: agent.clone(),
            number,
            work,
        })
        .collect()
}

/// Everything a review's prompt is built from.
#[derive(Clone, Copy, Debug)]
pub struct ReviewBrief<'a> {
    pub nickname: &'a str,
    pub number: u32,
    pub title: &'a str,
    pub head: &'a str,
    pub head_branch: &'a str,
    pub base_branch: &'a str,
    pub base_commit: &'a str,
    /// Files that conflict when the base is merged in; empty when it merged.
    pub conflicts: &'a [String],
    pub work: ReviewWork,
    /// The PR's review tier (F7 Tiered review), read from its diff against the base.
    pub tier: &'a TierAssessment,
    pub rules: &'a PrDutyRuleSettings,
    /// What you told PR duty from the Duty view's composer, if this review follows it.
    pub instruction: Option<&'a str>,
}

/// The review's prompt: F7's PR duty rules and severity bar, with the locked rules enforced by
/// the `pr_duty_review` / `pr_duty_merge` tools rather than trusted to the prompt.
pub fn review_prompt(brief: &ReviewBrief) -> String {
    let mut prompt = format!(
        "You are {}, on PR duty for this project (Velocity duty role: PR duty). You hold the review claim on PR #{} \"{}\" (branch `{}`): no other agent reviews, updates, approves or merges it while you hold it. {}\n\n",
        brief.nickname,
        brief.number,
        brief.title,
        brief.head_branch,
        work_sentence(brief)
    );
    prompt.push_str(&checkout_paragraph(brief));
    prompt.push_str(&tier_steps(brief));
    prompt.push_str(SEVERITY_BAR);
    if brief.rules.re_review_on_new_commits {
        prompt.push_str("- If an earlier head of this PR was reviewed, review only the commits pushed since; block only on findings in them, or on new data loss or a crash. A merge of the base branch that PR duty pushed is not a new commit to review; only its conflict resolutions are.\n");
    }
    prompt.push_str(&decision_rules(brief));
    if let Some(instruction) = brief.instruction {
        prompt.push_str(&format!("\nThe user asks PR duty: {instruction}\n"));
    }
    prompt.push_str("\nRun every command in the foreground and wait for it, however long it takes (a cold build, `lake build`, the stress suite). Never end your turn before you record your decision: ending it stops your session, and a command left running in the background never calls you back.\nEnd your turn with a one-line summary of your decision.\n");
    prompt
}

fn work_sentence(brief: &ReviewBrief) -> String {
    match brief.work {
        ReviewWork::Review => format!("Review its head {} now.", short_commit(brief.head)),
        ReviewWork::Merge => format!(
            "Its head {} is already approved in Velocity's record: run the local checks on the merge result and merge it if they pass. If they fail, request changes with the failing output.",
            short_commit(brief.head)
        ),
    }
}

fn checkout_paragraph(brief: &ReviewBrief) -> String {
    let base = brief.base_branch;
    if brief.conflicts.is_empty() {
        return format!(
            "This worktree is a detached checkout of the PR head {} with `{base}` ({}) merged in, so your checks run on the merge result, not the old PR head. The merge commit is local; do not push it unless you update the PR branch on purpose.\n",
            short_commit(brief.head),
            short_commit(brief.base_commit)
        );
    }
    format!(
        "Merging `{base}` ({}) into the PR conflicts in: {}. This worktree is checked out at the PR head {} alone. If both sides are clear and the resolution keeps both changes as written (imports, lists, neighbouring hunks, regenerated lockfiles), merge `{}` here, resolve it, push the merge commit to the PR branch (`git push origin HEAD:{}`, never force-push, never rewrite the author's commits) and review the new head. Otherwise request changes naming the conflicting files: the author resolves it.\n",
        short_commit(brief.base_commit),
        brief.conflicts.join(", "),
        short_commit(brief.head),
        brief.base_commit,
        brief.head_branch
    )
}

/// F7 Tiered review: a trivial PR gets the checks and a claim check; a normal PR the full
/// review; a sensitive PR the full review plus the stress suite and the strongest proof level.
fn tier_steps(brief: &ReviewBrief) -> String {
    match brief.tier.tier {
        ReviewTier::Trivial => format!(
            "\nTier: trivial. Velocity read its diff as docs, comments or formatting only. No proof is needed.\n- Read the PR with `pr_duty_pull_request` and confirm the change matches the piece its description claims. If you find a change that is not docs, comments or formatting, review it in full as a normal PR.\n{}- Approve once the local checks pass and the change matches its claim.\n",
            local_checks_step(brief.rules)
        ),
        ReviewTier::Normal => format!(
            "\nTier: normal. Full review.\n{}",
            review_steps(brief.rules)
        ),
        ReviewTier::Sensitive => format!(
            "\nTier: sensitive. It touches {}, listed under Sensitive paths in AGENTS.md. Full review, plus:\n- Run the project's full stress suite (its stress command in AGENTS.md) on the merge result before you merge. A failure blocks the merge.\n- Any proof-list feature it changes needs its confidence proof at the strongest level its language allows (SPEC.md F17). A weaker level blocks the merge.\n{}",
            quoted_list(&brief.tier.sensitive_files),
            review_steps(brief.rules)
        ),
    }
}

fn local_checks_step(rules: &PrDutyRuleSettings) -> &'static str {
    if rules.ci_green_before_merge {
        "- Run the project's local checks from AGENTS.md here, on the merge result. They decide, not GitHub CI: a pending, red or stale run never holds a PR up.\n"
    } else {
        ""
    }
}

fn quoted_list(files: &[String]) -> String {
    const SHOWN: usize = 8;
    let mut list: Vec<String> = files
        .iter()
        .take(SHOWN)
        .map(|file| format!("`{file}`"))
        .collect();
    if files.len() > SHOWN {
        list.push(format!("{} more", files.len() - SHOWN));
    }
    list.join(", ")
}

fn review_steps(rules: &PrDutyRuleSettings) -> String {
    let mut steps = String::from(
        "Review:\n- Read AGENTS.md and SPEC.md first and follow AGENTS.md's rules for PR duty. Read the PR with `pr_duty_pull_request`: its description names the piece it completes.\n- Review the intent, not each line: check the PR against its claim, its Task List item, issue or SPEC.md section, and AGENTS.md. Style is left to linters.\n",
    );
    if rules.check_diff_against_spec {
        steps.push_str("- Check the diff against SPEC.md: the main path of the claimed piece must do what its section says.\n");
    }
    steps.push_str(local_checks_step(rules));
    steps.push_str("- If `proofs/` changes, run `lake build` and the axiom audit. Never merge unless the proofs check.\n");
    steps.push_str("- Review every theorem statement, not the proofs: does it say what the SPEC.md section says, do its hypotheses exclude real inputs, is the cost model honest, and is the class of alternatives the one that matters?\n");
    steps.push_str("- Compare the claimed feature and changed runtime paths with the project proof list. A new proof-list feature needs Design.lean with exactly correct, cost_bound and optimal; off-list Lean or a theorem about one case gets changes requested. For a bug fix require the failing input in its confidence fixtures instead. Review confidence levels per language: reference by default, code only when requested, model flagged as a proof gap; sensitive changes require the strongest available level. Implementation must outweigh proofs, tests and docs together.\n");
    steps.push_str("- When you doubt a claim no proof covers, write a stress script, run it, and put its output in your review.\n");
    steps
}

const SEVERITY_BAR: &str = "\nSeverity bar (locked):\n- Block only for data loss or corruption, a crash or hang, a SPEC.md §1 rule violation, a security hole, a failing local check, or the main path of the claimed piece not doing what its SPEC.md section says.\n- File everything else as an issue with its evidence (`gh issue create`) and link it from an approving review.\n- Put every blocking finding in this review. When you are unsure whether a finding blocks, it does not block.\n";

fn decision_rules(brief: &ReviewBrief) -> String {
    let method = match brief.rules.merge_method {
        DutyMergeMethod::Squash => "squash",
        DutyMergeMethod::Rebase => "rebase",
        DutyMergeMethod::MergeCommit => "merge commit",
    };
    format!(
        "\nDecide (locked rules):\n- Record your decision only with `pr_duty_review`: action `approve` or `request_changes`, the head you checked (the PR head {}, or the new head if you pushed to the PR) and your reason, with `proof_review` explanations for scope against the proof list, theorem statements and confidence level (or why inapplicable), and inline comments in `comments`. Velocity's record of it is the approval Rule 4 requires; never approve, request changes or merge through `gh` or the GitHub website.\n- Before checking and again before merging call `pr_duty_prepare_landing`. Fetch and check out its returned head in this review worktree (never reset the author worktree), regenerate generated files and run the returned check scope. Report successful checks with `pr_duty_checked` using its exact head/base. If the base moves repeat: full checks for overlapping files, build and lint otherwise. Never skip a rebuild when the base moved. Conflicts go to the owning agent through request_changes naming the files when you cannot preserve both behaviours.\n- Never merge without an approving review. After you approve, merge with `pr_duty_merge` at the head you approved ({method}). A commit pushed after your approval needs a new review.\n- If the merge is refused, stop: Velocity records the failure.\n",
        short_commit(brief.head)
    )
}

/// The feed line for a claim (F7 activity feed).
pub fn claim_message(number: u32, title: &str, work: ReviewWork) -> String {
    match work {
        ReviewWork::Review => format!("Claimed #{number} {title} · reviewing"),
        ReviewWork::Merge => format!("Claimed #{number} {title} · merging the approved head"),
    }
}

/// The feed line for a review session that ended without merging or a decision. A session
/// that ended undecided rests its head for `UNDECIDED_RETRY_AFTER`, and the line says so.
pub fn review_finished_message(
    number: u32,
    decided: bool,
    failure: Option<&str>,
) -> Option<String> {
    match (failure, decided) {
        (Some(failure), _) => Some(format!(
            "Review of #{number} stopped · {}",
            step_doing(failure)
        )),
        (None, false) => Some(format!(
            "Released #{number} without a decision · retrying in {} min",
            UNDECIDED_RETRY_AFTER.as_secs() / 60
        )),
        (None, true) => None,
    }
}

/// A reviewing agent's card line: the PR and the agent's current step.
pub fn review_doing(number: u32, step: &str) -> String {
    step_doing(&format!("#{number} · {step}"))
}

/// What an idle PR duty card says.
pub fn review_idle_doing(paused: bool) -> String {
    if paused {
        "paused".into()
    } else {
        "idle · watching for PRs".into()
    }
}

pub fn short_commit(commit: &str) -> &str {
    commit.get(..7).unwrap_or(commit)
}
