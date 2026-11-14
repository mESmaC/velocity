//! Landing an approved head (SPEC.md F7 "Checks before merge"): Velocity reads GitHub's
//! checks on it and merges it itself once they pass, so no PR duty agent rebuilds a PR to
//! merge it.

use std::path::Path;

use velocity_core::merge_gate::{
    CheckVerdict, ChecksState, Landing, LandingFacts, check_run_verdict, checks_state, landing,
    status_context_verdict,
};

use crate::pull_request_queries::THREADS;
use crate::{
    MergeMethod, PullRequestCheck, PullRequestClient, PullRequestError, PullRequestOperation,
    PullRequestReviewThread, PullRequestState, ReviewRecord,
};

/// The repository's git, and where the project's statement locks are kept (F17).
#[derive(Clone, Copy, Debug)]
pub struct StatementLockPlace<'a> {
    /// Any checkout of the repository: statements are read from its git, not its files.
    pub checkout: &'a Path,
    pub app_data: &'a Path,
    pub project: &'a str,
}

/// Refuses a merge unless the theorem statements locked at approval are those at `head`
/// (F17 statement lock): a missing, unreadable or changed lock needs a fresh approval.
pub fn require_statement_lock(
    place: StatementLockPlace,
    number: u32,
    head: &str,
) -> Result<(), PullRequestError> {
    let held = (|| {
        let lock = velocity_proofs::load_statement_lock(place.app_data, place.project, number)?;
        let current = velocity_proofs::statements_at(place.checkout, head)?;
        Ok::<_, velocity_proofs::ProofCheckError>(
            lock.is_some_and(|lock| lock.changed_in(&current).is_empty()),
        )
    })();
    if matches!(held, Ok(true)) {
        return Ok(());
    }
    Err(PullRequestError::Invalid(
        "Statement lock missing, unreadable or changed: review and approve this head afresh".into(),
    ))
}

fn verdict(check: &PullRequestCheck) -> (String, CheckVerdict) {
    match check {
        PullRequestCheck::CheckRun {
            name,
            status,
            conclusion,
            ..
        } => (
            name.clone(),
            check_run_verdict(status, conclusion.as_deref()),
        ),
        PullRequestCheck::StatusContext { context, state, .. } => {
            (context.clone(), status_context_verdict(state))
        }
    }
}

impl ReviewRecord {
    /// The nick whose standing approval covers `head`, if one does.
    pub fn approved_by(&self, number: u32, head: &str) -> Option<String> {
        self.approver(number, head).ok()
    }

    /// Tells the PR's author that its approved head conflicts with `base_branch`: a
    /// `conflict` event (F6), which reaches the owning agent's thread.
    pub fn report_conflict(
        &self,
        number: u32,
        head: &str,
        base_branch: &str,
    ) -> Result<(), PullRequestError> {
        self.conflict(
            number,
            head,
            &format!("its approved head conflicts with `{base_branch}`"),
        )
    }
}

impl PullRequestClient {
    /// Whether GitHub reports any check on PR `number`'s head. A head with none is one only
    /// the local checks would build.
    pub fn has_checks(&self, number: u32) -> Result<bool, PullRequestError> {
        Ok(!self.checks(number)?.is_empty())
    }

    /// What to do with PR `number`'s approved `head` now. With `checks_required` false the
    /// role merges on approval, so GitHub's checks are not read.
    pub fn landing(
        &self,
        number: u32,
        head: &str,
        checks_required: bool,
    ) -> Result<Landing, PullRequestError> {
        let pull_request = self.get(number)?;
        let status = pull_request.merge_state_status.as_str();
        let mut facts = LandingFacts {
            current: pull_request.state == PullRequestState::Open
                && !pull_request.is_draft
                && pull_request.head_ref_oid == head,
            mergeability_known: status != "UNKNOWN",
            conflicts: status == "DIRTY",
            checks: ChecksState::Passed,
            open_threads: 0,
        };
        if !facts.current || !facts.mergeability_known || facts.conflicts {
            return Ok(landing(&facts));
        }
        if checks_required {
            let verdicts: Vec<_> = self.checks(number)?.iter().map(verdict).collect();
            facts.checks = checks_state(&verdicts);
        }
        if facts.checks == ChecksState::Passed {
            // Read last: only a head that would otherwise merge costs this request.
            facts.open_threads = self.open_threads(number)?;
        }
        Ok(landing(&facts))
    }

    /// The number of unresolved review threads on PR `number`.
    pub fn open_threads(&self, number: u32) -> Result<usize, PullRequestError> {
        let threads: Vec<PullRequestReviewThread> =
            self.pull_request_connection(number, "reviewThreads", THREADS)?;
        Ok(threads.iter().filter(|thread| !thread.is_resolved).count())
    }

    /// Merges PR `number` at its approved `head` on the approver's behalf. Refused, like a
    /// reviewer's own merge, without a standing approval at exactly `head` (Rule 4) or with a
    /// statement lock that no longer holds. `true` when the head branch was kept.
    pub fn land(
        &self,
        place: StatementLockPlace,
        number: u32,
        head: &str,
        method: MergeMethod,
    ) -> Result<bool, PullRequestError> {
        require_statement_lock(place, number, head)?;
        let receipt = self.execute(PullRequestOperation::Merge {
            number,
            expected_head: head.to_owned(),
            method,
        })?;
        Ok(receipt.operation == "merged_branch_kept")
    }
}
