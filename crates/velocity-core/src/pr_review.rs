//! PR duty reviews (SPEC.md F7 PR duty, Rule 4): which open PRs and Rosen changes need an
//! agent, which agent takes which (one each, so two agents never review the same one), what
//! the reviewing agent is told, and what its card and the Duty view's activity feed say.

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

/// What a PR duty agent reviews: a GitHub PR, or a Rosen change its thread has named ready
/// (F5). PRs come first, oldest first, then changes in their threads' order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewSubject {
    PullRequest(u32),
    Change { thread: u32, change: String },
}

impl ReviewSubject {
    /// How cards and feed lines name it: `#12`, or `change abcdefgh`.
    pub fn label(&self) -> String {
        match self {
            Self::PullRequest(number) => format!("#{number}"),
            Self::Change { change, .. } => format!("change {}", short_change(change)),
        }
    }

    /// The PR's number; `None` for a Rosen change.
    pub fn number(&self) -> Option<u32> {
        match self {
            Self::PullRequest(number) => Some(*number),
            Self::Change { .. } => None,
        }
    }
}

/// A Rosen change id's first eight characters, enough to tell changes apart in a project.
pub fn short_change(change: &str) -> &str {
    change.get(..8).unwrap_or(change)
}

/// An open PR or a Rosen change in review, as PR duty sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewCandidate {
    pub subject: ReviewSubject,
    pub title: String,
    pub head: String,
    pub head_branch: String,
    pub draft: bool,
    pub standing: ReviewStanding,
}

impl ReviewCandidate {
    /// Rosen change `change`, named ready by thread `thread`. Rosen holds its approvals: it
    /// is approved when one stands at exactly `head`.
    pub fn change(
        thread: u32,
        change: String,
        title: String,
        head: String,
        approved: bool,
    ) -> Self {
        Self {
            subject: ReviewSubject::Change { thread, change },
            title,
            head,
            head_branch: String::new(),
            draft: false,
            standing: if approved {
                ReviewStanding::Approved
            } else {
                ReviewStanding::Unreviewed
            },
        }
    }
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
pub fn continue_prompt(subject: &ReviewSubject, nudge: u32) -> String {
    let (named, tools) = match subject {
        ReviewSubject::PullRequest(number) => (
            format!("PR #{number}"),
            "`pr_duty_review` (and merge with `pr_duty_merge` once approved)",
        ),
        ReviewSubject::Change { .. } => (
            format!("Rosen {}", subject.label()),
            "`rosen_duty_review` (and land with `rosen_duty_land` once approved)",
        ),
    };
    format!(
        "You ended your turn without recording a decision on {named} (reminder {nudge} of {UNDECIDED_NUDGES}). Your session stops when a turn ends without one, so nothing you left running in the background will call you back. Continue the review now: run any long command in the foreground and wait for it, then record your decision with {tools}. If you cannot decide, say why in one line.\n"
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
    attempts: &BTreeMap<ReviewSubject, ReviewAttempt>,
    now: Duration,
) -> BTreeMap<ReviewSubject, String> {
    attempts
        .iter()
        .filter(|(_, attempt)| {
            attempt.decided || now.saturating_sub(attempt.ended_at) < UNDECIDED_RETRY_AFTER
        })
        .map(|(subject, attempt)| (subject.clone(), attempt.head.clone()))
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
    pub subject: ReviewSubject,
    pub work: ReviewWork,
}

/// Pairs idle agents with PRs and changes that need work, oldest first. One another agent
/// holds is never handed out, and each agent gets at most one (F7: one PR per agent).
pub fn assign_reviews(
    candidates: &[ReviewCandidate],
    idle_agents: &[String],
    held: &BTreeSet<ReviewSubject>,
    attempted: &BTreeMap<ReviewSubject, String>,
) -> Vec<ReviewAssignment> {
    let mut open: Vec<(ReviewSubject, ReviewWork)> = candidates
        .iter()
        .filter(|candidate| !held.contains(&candidate.subject))
        .filter_map(|candidate| {
            let tried = attempted.get(&candidate.subject).map(String::as_str);
            review_work(candidate, tried).map(|work| (candidate.subject.clone(), work))
        })
        .collect();
    open.sort_by(|(left, _), (right, _)| left.cmp(right));
    open.dedup_by(|(left, _), (right, _)| left == right);
    idle_agents
        .iter()
        .zip(open)
        .map(|(agent, (subject, work))| ReviewAssignment {
            agent: agent.clone(),
            subject,
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
/// review; a sensitive PR the full review plus the project's stress suite and reference-level
/// confidence proofs (code level only when an issue asks for it, F17).
fn tier_steps(brief: &ReviewBrief) -> String {
    match brief.tier.tier {
        ReviewTier::Trivial => format!(
            "\nTier: trivial. Velocity read its diff as docs, comments or formatting only. No proof is needed.\n{CONVERSATION_STEP}- Read the PR with `pr_duty_pull_request` and confirm the change matches the piece its description claims. If you find a change that is not docs, comments or formatting, review it in full as a normal PR.\n{}- Approve once the local checks pass and the change matches its claim.\n",
            local_checks_step(brief.rules)
        ),
        ReviewTier::Normal => format!(
            "\nTier: normal. Full review.\n{}",
            review_steps(brief.rules)
        ),
        ReviewTier::Sensitive => format!(
            "\nTier: sensitive. It touches {}, listed under Sensitive paths in AGENTS.md. Full review, plus:\n- Run the project's stress suite, the stress command AGENTS.md names as the merge gate, on the merge result before you merge. A failure blocks the merge. Never run a benchmark or load run that AGENTS.md marks as not a merge gate.\n- Any proof-list feature it changes needs its confidence proof at the reference level: a fixture table exported from the Lean model plus a test that checks the code against it (SPEC.md F17). A model-only level blocks the merge. The code level (a translation) is required only when an issue asks for one.\n{}",
            quoted_list(&brief.tier.sensitive_files),
            review_steps(brief.rules)
        ),
    }
}

/// Comments are how you steer a review from GitHub (F7): read before deciding, and a hold
/// rests the head instead of being reviewed past.
const CONVERSATION_STEP: &str = "- Before anything else, read the PR's whole conversation in `pr_duty_pull_request`: every comment, review and review thread, newest last. A comment from a person (not a PR duty agent's review) is an instruction for this review: follow it. If one puts the PR on hold, says not to review or merge it yet, or makes it wait for other work, and nothing newer lifts that, call `pr_duty_review` with action `hold`, the head and the comment's reason, then stop: run no checks.\n";

pub(crate) fn local_checks_step(rules: &PrDutyRuleSettings) -> &'static str {
    if rules.ci_green_before_merge {
        "- Run the project's local checks from AGENTS.md here, on the merge result. They decide, not GitHub CI: a pending, red or stale run never holds a PR up.\n"
    } else {
        ""
    }
}

pub(crate) fn quoted_list(files: &[String]) -> String {
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
    steps.push_str(CONVERSATION_STEP);
    if rules.check_diff_against_spec {
        steps.push_str("- Check the diff against SPEC.md: the main path of the claimed piece must do what its section says.\n");
    }
    steps.push_str(local_checks_step(rules));
    steps.push_str("- If `proofs/` changes, run `lake build` and the axiom audit. Never merge unless the proofs check.\n");
    steps.push_str("- Review every theorem statement, not the proofs: does it say what the SPEC.md section says, do its hypotheses exclude real inputs, is the cost model honest, and is the class of alternatives the one that matters?\n");
    steps.push_str("- Compare the claimed feature and changed runtime paths with the project proof list. A new proof-list feature needs Design.lean with exactly correct, cost_bound and optimal; off-list Lean or a theorem about one case gets changes requested. For a bug fix require the failing input in its confidence fixtures instead. Review confidence levels per language: reference by default, code only when requested, model flagged as a proof gap; sensitive changes require at least the reference level. Implementation must outweigh proofs, tests and docs together.\n");
    steps.push_str("- When you doubt a claim no proof covers, write a stress script, run it, and put its output in your review.\n");
    steps
}

pub(crate) const SEVERITY_BAR: &str = "\nSeverity bar (locked):\n- Block only for data loss or corruption, a crash or hang, a SPEC.md §1 rule violation, a security hole, a failing local check, or the main path of the claimed piece not doing what its SPEC.md section says.\n- File everything else as an issue with its evidence (`gh issue create`) and link it from an approving review.\n- Put every blocking finding in this review. When you are unsure whether a finding blocks, it does not block.\n";

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
pub fn claim_message(subject: &ReviewSubject, title: &str, work: ReviewWork) -> String {
    let label = subject.label();
    match work {
        ReviewWork::Review => format!("Claimed {label} {title} · reviewing"),
        ReviewWork::Merge => format!("Claimed {label} {title} · merging the approved head"),
    }
}

/// The feed line for a review session that ended without merging or a decision. A session
/// that ended undecided rests its head for `UNDECIDED_RETRY_AFTER`, and the line says so.
pub fn review_finished_message(
    subject: &ReviewSubject,
    decided: bool,
    failure: Option<&str>,
) -> Option<String> {
    let label = subject.label();
    match (failure, decided) {
        (Some(failure), _) => Some(format!(
            "Review of {label} stopped · {}",
            step_doing(failure)
        )),
        (None, false) => Some(format!(
            "Released {label} without a decision · retrying in {} min",
            UNDECIDED_RETRY_AFTER.as_secs() / 60
        )),
        (None, true) => None,
    }
}

/// A reviewing agent's card line: the PR or change and the agent's current step.
pub fn review_doing(subject: &ReviewSubject, step: &str) -> String {
    step_doing(&format!("{} · {step}", subject.label()))
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
