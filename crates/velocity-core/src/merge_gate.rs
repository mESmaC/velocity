//! What gates a PR's merge (SPEC.md F7 "Checks before merge"): whether PR duty reviews a PR by
//! reading it or by running the local checks, how GitHub's checks on a head add up, and what
//! Velocity does with an approved head. Pure decisions; the GitHub reads live in
//! `velocity-github`.

use crate::review_tier::ReviewTier;

/// How one review is done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewMode {
    /// The agent reads the PR and records its decision. It builds and tests nothing: GitHub's
    /// checks do, and Velocity merges the approved head once they pass.
    Reading,
    /// The agent runs the project's local checks on the merge result and merges itself.
    LocalChecks,
}

/// What a review's mode is decided from.
#[derive(Clone, Copy, Debug)]
pub struct ModeEvidence {
    pub tier: ReviewTier,
    /// The role's "Checks pass" rule: a merge waits for checks.
    pub checks_required: bool,
    /// Something other than the agent checks the head: GitHub reports at least one check
    /// on the PR's head, or a Rosen project names pre-land commands for Velocity to run.
    pub has_github_checks: bool,
    /// The PR changes a file in the Lake project.
    pub changes_proofs: bool,
}

/// A review is by reading unless something only a local run can check is at stake: a
/// sensitive path's stress gate, a changed proof, or a head nothing else would build.
pub fn review_mode(evidence: ModeEvidence) -> ReviewMode {
    let unchecked = evidence.checks_required && !evidence.has_github_checks;
    if evidence.tier == ReviewTier::Sensitive || evidence.changes_proofs || unchecked {
        ReviewMode::LocalChecks
    } else {
        ReviewMode::Reading
    }
}

/// Where one GitHub check leaves a head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckVerdict {
    Passed,
    Pending,
    Failed,
}

/// A check run passes when it completed as success, neutral or skipped. Neutral is how a
/// review bot says "a person should look", which Velocity's own approval already answers.
pub fn check_run_verdict(status: &str, conclusion: Option<&str>) -> CheckVerdict {
    if !status.eq_ignore_ascii_case("COMPLETED") {
        return CheckVerdict::Pending;
    }
    match conclusion.map(str::to_ascii_uppercase).as_deref() {
        Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => CheckVerdict::Passed,
        None => CheckVerdict::Pending,
        Some(_) => CheckVerdict::Failed,
    }
}

/// A commit status passes only as success.
pub fn status_context_verdict(state: &str) -> CheckVerdict {
    match state.to_ascii_uppercase().as_str() {
        "SUCCESS" => CheckVerdict::Passed,
        "PENDING" | "EXPECTED" => CheckVerdict::Pending,
        _ => CheckVerdict::Failed,
    }
}

/// Where all of a head's GitHub checks leave it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChecksState {
    /// GitHub reports no check on the head.
    None,
    Pending,
    Passed,
    /// The names of the failed checks.
    Failed(Vec<String>),
}

/// One failed check fails the head at once, even while others still run: its author has to
/// push either way.
pub fn checks_state(checks: &[(String, CheckVerdict)]) -> ChecksState {
    let failed: Vec<String> = checks
        .iter()
        .filter(|(_, verdict)| *verdict == CheckVerdict::Failed)
        .map(|(name, _)| name.clone())
        .collect();
    if !failed.is_empty() {
        ChecksState::Failed(failed)
    } else if checks.is_empty() {
        ChecksState::None
    } else if checks
        .iter()
        .any(|(_, verdict)| *verdict == CheckVerdict::Pending)
    {
        ChecksState::Pending
    } else {
        ChecksState::Passed
    }
}

/// An approved head as GitHub has it now.
#[derive(Clone, Debug)]
pub struct LandingFacts {
    /// The PR is open, ready and still at the approved head.
    pub current: bool,
    /// GitHub has worked out whether the PR merges cleanly.
    pub mergeability_known: bool,
    pub conflicts: bool,
    pub checks: ChecksState,
    pub open_threads: usize,
}

/// What Velocity does with an approved head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    Merge,
    /// Nothing to say yet: checks are running, or GitHub is still working out mergeability.
    Pending,
    /// No check is reported on the head.
    NoChecks,
    ChecksFailed(Vec<String>),
    Conflicts,
    OpenThreads(usize),
    /// The PR closed, merged, became a draft or moved to a new head.
    Gone,
}

/// Checks come before review threads: a failed check sends the PR back to its author, who
/// answers the threads in the same push.
pub fn landing(facts: &LandingFacts) -> Landing {
    if !facts.current {
        return Landing::Gone;
    }
    if !facts.mergeability_known {
        return Landing::Pending;
    }
    if facts.conflicts {
        return Landing::Conflicts;
    }
    match &facts.checks {
        ChecksState::Failed(names) => Landing::ChecksFailed(names.clone()),
        ChecksState::Pending => Landing::Pending,
        ChecksState::None => Landing::NoChecks,
        ChecksState::Passed if facts.open_threads > 0 => Landing::OpenThreads(facts.open_threads),
        ChecksState::Passed => Landing::Merge,
    }
}

/// The feed line for an approved head that can't merge yet; `None` while there is nothing
/// for anyone to do but wait.
pub fn landing_wait_message(number: u32, base_branch: &str, landing: &Landing) -> Option<String> {
    let why = match landing {
        Landing::ChecksFailed(names) => {
            const SHOWN: usize = 3;
            let mut shown: Vec<&str> = names.iter().take(SHOWN).map(String::as_str).collect();
            if names.len() > SHOWN {
                shown.push("…");
            }
            format!(
                "GitHub checks failed ({}); its author fixes and pushes",
                shown.join(", ")
            )
        }
        Landing::Conflicts => {
            format!("it conflicts with `{base_branch}`; its author resolves it")
        }
        Landing::OpenThreads(1) => "1 review thread is open".to_owned(),
        Landing::OpenThreads(count) => format!("{count} review threads are open"),
        Landing::Merge | Landing::Pending | Landing::NoChecks | Landing::Gone => return None,
    };
    Some(format!("Approved #{number} waits · {why}"))
}

/// The feed line for a head Velocity merged.
pub fn landed_message(number: u32, checks_required: bool, branch_kept: bool) -> String {
    let when = if checks_required {
        "once GitHub's checks passed"
    } else {
        "on approval"
    };
    let branch = if branch_kept {
        "branch kept"
    } else {
        "branch deleted"
    };
    format!("Merged #{number} {when} · {branch}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(checks: ChecksState) -> LandingFacts {
        LandingFacts {
            current: true,
            mergeability_known: true,
            conflicts: false,
            checks,
            open_threads: 0,
        }
    }

    // The risk: Velocity merges an approved head that a check failed on, that is still being
    // checked, or that nothing checked, since no agent builds it any more.
    #[test]
    fn only_a_head_whose_checks_all_passed_merges() {
        let named = |verdict| ("build".to_owned(), verdict);
        let passed = named(CheckVerdict::Passed);
        let mixed = [passed.clone(), named(CheckVerdict::Pending)];
        let failed = [passed.clone(), named(CheckVerdict::Failed)];
        assert_eq!(landing(&facts(checks_state(&[passed]))), Landing::Merge);
        assert_eq!(landing(&facts(checks_state(&mixed))), Landing::Pending);
        assert_eq!(landing(&facts(checks_state(&[]))), Landing::NoChecks);
        assert_eq!(
            landing(&facts(checks_state(&failed))),
            Landing::ChecksFailed(vec!["build".to_owned()])
        );
        for conclusion in ["FAILURE", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED"] {
            assert_eq!(
                check_run_verdict("COMPLETED", Some(conclusion)),
                CheckVerdict::Failed
            );
        }
        assert_eq!(
            check_run_verdict("IN_PROGRESS", None),
            CheckVerdict::Pending
        );
        assert_eq!(status_context_verdict("ERROR"), CheckVerdict::Failed);
    }
}
