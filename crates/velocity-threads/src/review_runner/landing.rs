//! Landing approved PRs (SPEC.md F7 "Checks before merge"). A review by reading ends at its
//! approval, so Velocity lands the head itself: each tick it reads GitHub's checks on every
//! approved head no agent holds and merges the ones that passed. No agent rebuilds a PR to
//! merge it, and a merge never restarts another PR's checks.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use velocity_core::duty::{DutyEventKind, DutyFeedEvent, DutyMergeMethod};
use velocity_core::merge_gate::{
    Landing, ModeEvidence, ReviewMode, landed_message, landing_wait_message, review_mode,
};
use velocity_core::pr_review::{ReviewCandidate, ReviewStanding, ReviewWork};
use velocity_core::review_tier::ReviewTier;
use velocity_git::ReviewCheckout;
use velocity_github::{
    MergeMethod, PullRequestClient, PullRequestError, ReviewRecord, StatementLockPlace,
};
use velocity_store::duty::PrDutyRoster;

use super::{PrDutyReviews, ReviewHost};
use crate::review::review_worktree;

/// How long an approved head with no GitHub check waits for one to be reported. After that
/// nothing else will build it, so an agent runs the local checks and merges it.
const NO_CHECKS_GRACE: Duration = Duration::from_secs(120);
/// The nick Velocity reads the approval record under when it lands a head.
pub(super) const LANDER: &str = "pr-duty";

/// What Velocity last saw and said about one approved head.
pub(super) struct LandingNote {
    head: String,
    first_seen: Duration,
    /// The last wait line posted to the feed, so each is posted once.
    said: Option<String>,
    /// No GitHub check ever arrived: an agent merges this head after the local checks.
    left_to_agent: bool,
}

/// One look at an approved head.
enum Step {
    Merged { approver: String, branch_kept: bool },
    Waiting { approver: String, landing: Landing },
    Refused { approver: String, reason: String },
}

impl PrDutyReviews {
    /// How `work` on a prepared checkout is reviewed (blocking: it asks GitHub for the
    /// head's checks). Evidence that can't be read counts toward the local checks.
    pub(super) fn review_mode(
        &self,
        roster: &PrDutyRoster,
        number: u32,
        work: ReviewWork,
        checkout: &ReviewCheckout,
        tier: ReviewTier,
    ) -> ReviewMode {
        if work == ReviewWork::Merge {
            return ReviewMode::LocalChecks;
        }
        let checks_required = roster.rules.ci_green_before_merge;
        let repository = PathBuf::from(&roster.project.repository_path);
        let has_github_checks = checks_required
            && self
                .client(&repository)
                .and_then(|client| client.has_checks(number))
                .unwrap_or(false);
        let changes_proofs =
            velocity_proofs::changes_proofs(&checkout.path, &checkout.base_commit, &checkout.head)
                .unwrap_or(true);
        review_mode(ModeEvidence {
            tier,
            checks_required,
            has_github_checks,
            changes_proofs,
        })
    }

    /// Starts landing the approved heads no agent holds, and returns the candidates left for
    /// agents. One landing pass runs per project at a time, off the tick.
    pub(super) fn land_approved<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        candidates: Vec<ReviewCandidate>,
    ) -> Vec<ReviewCandidate> {
        let project = roster.project.id;
        let (approved, rest, start) = {
            let mut state = self.lock();
            let held: BTreeSet<u32> = state
                .running
                .iter()
                .filter(|((owner, _), running)| *owner == project && !running.dismissed)
                .filter_map(|(_, running)| running.subject.number())
                .collect();
            // A note belongs to one approved head: a new head or a new standing starts afresh.
            state.landing.retain(|(owner, number), note| {
                *owner != project
                    || candidates.iter().any(|candidate| {
                        pr_number(candidate) == *number
                            && candidate.head == note.head
                            && candidate.standing == ReviewStanding::Approved
                    })
            });
            let (approved, rest): (Vec<_>, Vec<_>) =
                candidates.into_iter().partition(|candidate| {
                    // A Rosen change lands through Rosen (`checker`), never here.
                    candidate.subject.number().is_some()
                        && candidate.standing == ReviewStanding::Approved
                        && !candidate.draft
                        && !held.contains(&pr_number(candidate))
                        && !state
                            .landing
                            .get(&(project, pr_number(candidate)))
                            .is_some_and(|note| note.left_to_agent)
                });
            let start = !approved.is_empty() && state.landing_projects.insert(project);
            (approved, rest, start)
        };
        if start {
            let (runner, host, roster) = (self.clone(), host.clone(), roster.clone());
            tokio::spawn(async move {
                for candidate in &approved {
                    runner.land_one(&host, &roster, candidate).await;
                }
                runner.lock().landing_projects.remove(&project);
            });
        }
        rest
    }

    async fn land_one<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        candidate: &ReviewCandidate,
    ) {
        let (runner, blocking_roster, blocking_candidate) =
            (self.clone(), roster.clone(), candidate.clone());
        let step = tokio::task::spawn_blocking(move || {
            runner.look_at(&blocking_roster, &blocking_candidate)
        })
        .await;
        match step {
            Ok(Ok(Step::Merged {
                approver,
                branch_kept,
            })) => {
                self.landed(host, roster, pr_number(candidate), &approver, branch_kept)
                    .await;
            }
            Ok(Ok(Step::Waiting { approver, landing })) => {
                self.waits(host, roster, candidate, &approver, &landing);
            }
            Ok(Ok(Step::Refused { approver, reason })) => {
                let message = velocity_core::scout_sweep::step_doing(&format!(
                    "Merge of #{} refused · {reason}",
                    pr_number(candidate)
                ));
                self.say_once(host, roster.project.id, candidate, &approver, message);
            }
            // GitHub could not be read: the next tick looks again.
            Ok(Err(_)) | Err(_) => {}
        }
    }

    /// Reads where the approved head stands and merges it when nothing is in the way
    /// (blocking GitHub calls).
    fn look_at(
        &self,
        roster: &PrDutyRoster,
        candidate: &ReviewCandidate,
    ) -> Result<Step, PullRequestError> {
        let repository = PathBuf::from(&roster.project.repository_path);
        let project = roster.project.id.to_string();
        let (number, head) = (pr_number(candidate), candidate.head.as_str());
        let record = ReviewRecord::open(&self.app_data, &project, LANDER)?;
        let approver = record
            .approved_by(number, head)
            .unwrap_or_else(|| LANDER.to_owned());
        let checks_required = roster.rules.ci_green_before_merge;
        let landing = self
            .client(&repository)?
            .landing(number, head, checks_required)?;
        if landing != Landing::Merge {
            return Ok(Step::Waiting { approver, landing });
        }
        let place = StatementLockPlace {
            checkout: &repository,
            app_data: &self.app_data,
            project: &project,
        };
        let method = match roster.rules.merge_method {
            DutyMergeMethod::Squash => MergeMethod::Squash,
            DutyMergeMethod::Rebase => MergeMethod::Rebase,
            DutyMergeMethod::MergeCommit => MergeMethod::Merge,
        };
        // The kept client carries no approval record; the merge needs one (Rule 4).
        let lander = PullRequestClient::discover(&repository)?.with_review_record(record);
        Ok(match lander.land(place, number, head, method) {
            Ok(branch_kept) => Step::Merged {
                approver,
                branch_kept,
            },
            Err(error) => Step::Refused {
                approver,
                reason: error.to_string(),
            },
        })
    }

    /// The head merged: its feed line, and its review worktree goes (Rule 5).
    async fn landed<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        number: u32,
        approver: &str,
        branch_kept: bool,
    ) {
        let project = roster.project.id;
        self.lock().landing.remove(&(project, number));
        let mut removed = None;
        if let Some(worktree) = review_worktree(roster, number) {
            let repository = PathBuf::from(&roster.project.repository_path);
            let path = worktree.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                velocity_git::remove_review_worktree(&repository, &path)
            })
            .await;
            if matches!(outcome, Ok(Ok(()))) {
                removed = Some(worktree.display().to_string());
            }
        }
        let message = landed_message(number, roster.rules.ci_green_before_merge, branch_kept);
        let approver = approver.to_owned();
        host.update(
            project,
            Box::new(move |entry, timestamp| {
                if let Some(removed) = &removed {
                    for card in &mut entry.agents {
                        card.review_worktrees.retain(|known| known != removed);
                    }
                }
                vec![DutyFeedEvent::new(
                    timestamp,
                    &approver,
                    DutyEventKind::Merge,
                    message,
                )]
            }),
        );
        self.wake();
    }

    /// The head can't merge yet: says why once, tells its author about a conflict, and
    /// hands a head no check ever arrived for to an agent.
    fn waits<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        candidate: &ReviewCandidate,
        approver: &str,
        landing: &Landing,
    ) {
        let project = roster.project.id;
        if *landing == Landing::Gone {
            return;
        }
        if *landing == Landing::NoChecks && self.leave_to_agent(project, candidate) {
            return self.wake();
        }
        let base_branch = crate::launch::base_branch(&roster.project);
        let Some(message) = landing_wait_message(pr_number(candidate), &base_branch, landing)
        else {
            self.note(project, candidate);
            return;
        };
        if !self.say_once(host, project, candidate, approver, message) {
            return;
        }
        if *landing == Landing::Conflicts {
            let app_data = self.app_data.clone();
            let (number, head) = (pr_number(candidate), candidate.head.clone());
            // Appends one line to the project's AIRC channel; nothing waits on it.
            tokio::task::spawn_blocking(move || {
                if let Ok(record) = ReviewRecord::open(&app_data, &project.to_string(), LANDER) {
                    let _ = record.report_conflict(number, &head, &base_branch);
                }
            });
        }
    }

    /// Whether the head has waited out `NO_CHECKS_GRACE` with no check: it then stops
    /// resting, so the next idle agent takes it as merge work.
    fn leave_to_agent(&self, project: u32, candidate: &ReviewCandidate) -> bool {
        let now = self.epoch.elapsed();
        let mut state = self.lock();
        let note = state
            .landing
            .entry((project, pr_number(candidate)))
            .or_insert_with(|| LandingNote::new(&candidate.head, now));
        if now.saturating_sub(note.first_seen) < NO_CHECKS_GRACE {
            return false;
        }
        note.left_to_agent = true;
        if let Some(attempts) = state.attempted.get_mut(&project) {
            attempts.remove(&candidate.subject);
        }
        true
    }

    fn note(&self, project: u32, candidate: &ReviewCandidate) {
        let now = self.epoch.elapsed();
        self.lock()
            .landing
            .entry((project, pr_number(candidate)))
            .or_insert_with(|| LandingNote::new(&candidate.head, now));
    }

    /// Posts `message` to the feed unless it was the last thing said about this head.
    /// `true` when it was posted.
    fn say_once<Host: ReviewHost>(
        &self,
        host: &Host,
        project: u32,
        candidate: &ReviewCandidate,
        approver: &str,
        message: String,
    ) -> bool {
        let now = self.epoch.elapsed();
        {
            let mut state = self.lock();
            let note = state
                .landing
                .entry((project, pr_number(candidate)))
                .or_insert_with(|| LandingNote::new(&candidate.head, now));
            if note.said.as_deref() == Some(message.as_str()) {
                return false;
            }
            note.said = Some(message.clone());
        }
        let approver = approver.to_owned();
        host.update(
            project,
            Box::new(move |_, timestamp| {
                vec![DutyFeedEvent::new(
                    timestamp,
                    &approver,
                    DutyEventKind::Wait,
                    message,
                )]
            }),
        );
        true
    }
}

impl LandingNote {
    fn new(head: &str, now: Duration) -> Self {
        Self {
            head: head.to_owned(),
            first_seen: now,
            said: None,
            left_to_agent: false,
        }
    }
}

/// The PR number of a candidate this module lands: only PRs reach it (`land_approved`).
fn pr_number(candidate: &ReviewCandidate) -> u32 {
    candidate.subject.number().unwrap_or_default()
}
