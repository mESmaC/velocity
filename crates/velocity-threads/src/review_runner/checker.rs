//! Checking and landing approved Rosen changes (SPEC.md F7 "Checks before merge"; Rosen R8
//! and §9 "Build queue"). A review by reading ends at its approval, so Velocity does the rest
//! itself: it runs the project's pre-land commands on the change's merge result, submits the
//! receipt and asks Rosen to land.
//!
//! One pass runs per project, one change at a time. Each land moves the base under every
//! other receipt, so a second checker would only build merge results that are stale by the
//! time it finishes; one checker's receipt is always for the base it lands on.

use std::collections::BTreeSet;
use std::path::PathBuf;

use velocity_core::duty::{DutyEventKind, DutyFeedEvent};
use velocity_core::merge_gate::ReviewMode;
use velocity_core::pr_review::{ReviewCandidate, ReviewStanding, ReviewSubject, ReviewWork};
use velocity_core::rosen_checker::{
    FailedCheck, LandRefusal, check_failed_line, check_failed_message, checking_line,
    conflict_line, conflict_message, land_refusal, landed_line, refused_line,
};
use velocity_rosen::ProjectServer;
use velocity_rosen::checker::{self, CHECKER};
use velocity_rosen::duty::{self, LandOutcome};
use velocity_store::duty::PrDutyRoster;

use super::landing::LANDER;
use super::{PrDutyReviews, ReviewHost};
use crate::launch::Location;

/// How often one change is checked again in a pass because the base moved under its
/// receipt. Only another lander moves it, so more rounds than this mean the change is
/// better tried again on the next tick.
const ROUNDS: u32 = 3;
/// The project's folder the checks build in, beside its agents' workspaces.
const CHECK_FOLDER: &str = "pre-land-checks";

/// What Velocity last saw and said about one approved change.
pub(super) struct CheckNote {
    head: String,
    /// The last line posted to the feed about it, so each is posted once.
    said: Option<String>,
    /// Velocity can't check this head itself: an agent runs the local checks and lands it.
    left_to_agent: bool,
}

/// One approved change, while the pass works on it.
struct Check<'a, Host> {
    runner: &'a PrDutyReviews,
    host: &'a Host,
    roster: &'a PrDutyRoster,
    server: ProjectServer,
    thread: u32,
    change: &'a str,
    /// The head the approval stands at.
    head: &'a str,
}

impl PrDutyReviews {
    /// Starts checking and landing the approved changes no agent holds, and returns the
    /// candidates left for agents. One pass runs per project at a time, off the tick.
    pub(super) fn check_approved<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        candidates: Vec<ReviewCandidate>,
    ) -> Vec<ReviewCandidate> {
        let project = roster.project.id;
        let (approved, rest, start) = {
            let mut state = self.lock();
            let held: BTreeSet<ReviewSubject> = state
                .running
                .iter()
                .filter(|((owner, _), running)| *owner == project && !running.dismissed)
                .map(|(_, running)| running.subject.clone())
                .collect();
            // A note belongs to one approved head: a new head or standing starts afresh.
            state.checking.retain(|(owner, change), note| {
                *owner != project
                    || candidates.iter().any(|candidate| {
                        change_of(candidate) == Some(change.as_str())
                            && candidate.head == note.head
                            && candidate.standing == ReviewStanding::Approved
                    })
            });
            let (approved, rest): (Vec<_>, Vec<_>) =
                candidates.into_iter().partition(|candidate| {
                    change_of(candidate).is_some_and(|change| {
                        candidate.standing == ReviewStanding::Approved
                            && !held.contains(&candidate.subject)
                            && !state
                                .checking
                                .get(&(project, change.to_owned()))
                                .is_some_and(|note| note.left_to_agent)
                    })
                });
            let start = !approved.is_empty() && state.checking_projects.insert(project);
            (approved, rest, start)
        };
        if start {
            let (runner, host, roster) = (self.clone(), host.clone(), roster.clone());
            tokio::spawn(async move {
                for candidate in &approved {
                    runner.check_one(&host, &roster, candidate).await;
                }
                runner.lock().checking_projects.remove(&project);
            });
        }
        rest
    }

    async fn check_one<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        candidate: &ReviewCandidate,
    ) {
        let ReviewSubject::Change { thread, change } = &candidate.subject else {
            return;
        };
        let server = ProjectServer::new(&self.app_data, roster.project.id);
        let repository = PathBuf::from(&roster.project.repository_path);
        let mode =
            crate::rosen_review::change_mode(&server, &repository, change, ReviewWork::Review);
        match mode.await {
            Ok((_, ReviewMode::Reading)) => {}
            Ok((_, ReviewMode::LocalChecks)) => {
                return self.leave_change_to_agent(roster.project.id, candidate);
            }
            // Rosen could not be read: the next tick looks again.
            Err(_) => return,
        }
        let check = Check {
            runner: self,
            host,
            roster,
            server,
            thread: *thread,
            change,
            head: &candidate.head,
        };
        check.run().await;
    }

    /// An approved head Velocity can't check itself (a sensitive path, a changed proof, or
    /// a project with no pre-land commands) stops resting, so the next idle agent takes it,
    /// runs the local checks and lands it.
    fn leave_change_to_agent(&self, project: u32, candidate: &ReviewCandidate) {
        let Some(change) = change_of(candidate) else {
            return;
        };
        let mut state = self.lock();
        let note = state
            .checking
            .entry((project, change.to_owned()))
            .or_insert_with(|| CheckNote::new(&candidate.head));
        note.left_to_agent = true;
        if let Some(attempts) = state.attempted.get_mut(&project) {
            attempts.remove(&candidate.subject);
        }
        drop(state);
        self.wake();
    }
}

impl<Host: ReviewHost> Check<'_, Host> {
    /// Asks Rosen to land the change, checking it first whenever Rosen wants a receipt for
    /// the current merge result. Asking before checking lands a prose-only change, or one
    /// whose receipt still counts, without a build.
    async fn run(&self) {
        let mut checked = false;
        for _ in 0..ROUNDS {
            let (code, message) = match duty::land(&self.server, CHECKER, self.change).await {
                Ok(LandOutcome::Landed { commit }) => return self.landed(&commit, checked),
                Ok(LandOutcome::Refused { code, message }) => (code, message),
                // Rosen could not be reached: the next tick asks again.
                Err(_) => return,
            };
            match land_refusal(&code) {
                // A conflict is found, with its files, when the merge result is staged.
                LandRefusal::Receipt | LandRefusal::Conflict => {
                    if !self.check().await {
                        return;
                    }
                    checked = true;
                }
                // The author moved the head: PR duty reviews the new one.
                LandRefusal::Approval => return,
                LandRefusal::Other => {
                    let reason = format!("{code}: {message}");
                    self.say_once(DutyEventKind::Wait, refused_line(self.change, &reason));
                    return;
                }
            }
        }
    }

    /// Runs the pre-land commands on the change's current merge result and submits the
    /// receipt. `false` when there is no receipt: the change went back to its author, moved,
    /// or could not be checked this time.
    async fn check(&self) -> bool {
        let Some(folder) = self.folder() else {
            return false;
        };
        let commands = match checker::pre_land_commands(&self.server).await {
            // No commands would be a receipt for nothing: an agent checks it instead.
            Ok(commands) if !commands.is_empty() => commands,
            _ => return false,
        };
        let staged = match checker::stage_merge_result(&self.server, self.change, &folder).await {
            Ok(staged) => staged,
            Err(error) => return self.could_not_check(&error.to_string()),
        };
        if !staged.conflicts.is_empty() {
            let base_branch = crate::launch::base_branch(&self.roster.project);
            let message = conflict_message(self.change, &base_branch, &staged.conflicts);
            self.send_back(&message, conflict_line(self.change, &base_branch));
            return false;
        }
        if staged.head != self.head {
            // A new head since the approval: Rosen has voided it.
            return false;
        }
        self.post(
            DutyEventKind::Wait,
            checking_line(self.change, &staged.base),
        );
        let ran = tokio::task::spawn_blocking(move || {
            checker::apply_staged(&folder)?;
            checker::run_commands(&folder, &commands)
        })
        .await;
        match ran {
            Ok(Ok(None)) => self.submit_receipt(&staged.head, &staged.base).await,
            Ok(Ok(Some(failed))) => {
                let failed = FailedCheck {
                    command: failed.command,
                    exit_code: failed.exit_code,
                    output: failed.output,
                };
                let message = check_failed_message(self.change, &staged.base, &failed);
                self.send_back(&message, check_failed_line(self.change, &failed.command));
                false
            }
            Ok(Err(error)) => self.could_not_check(&error.to_string()),
            Err(error) => self.could_not_check(&error.to_string()),
        }
    }

    async fn submit_receipt(&self, head: &str, base: &str) -> bool {
        match duty::submit_receipt(&self.server, CHECKER, self.change, (head, base)).await {
            Ok(()) => true,
            Err(error) => self.could_not_check(&format!("Rosen refused the receipt: {error}")),
        }
    }

    /// The project's check folder, beside its agents' workspaces.
    fn folder(&self) -> Option<PathBuf> {
        Location::new(&self.roster.project, &self.roster.worktrees)
            .project_root()
            .ok()
            .map(|root| root.join(CHECK_FOLDER))
    }

    /// The checks could not run at all, which is no verdict on the change: it stays
    /// approved, the feed says why once, and the next tick tries again.
    fn could_not_check(&self, why: &str) -> bool {
        let reason = format!("the pre-land checks could not run: {why}");
        self.say_once(DutyEventKind::Wait, refused_line(self.change, &reason));
        false
    }

    fn landed(&self, commit: &str, checked: bool) {
        let project = self.roster.project.id;
        self.runner
            .lock()
            .checking
            .remove(&(project, self.change.to_owned()));
        self.post(
            DutyEventKind::Merge,
            landed_line(self.change, commit, checked),
        );
        // The next approved change is checked on the base this land made.
        self.runner.wake();
    }

    /// Hands the change back to its author's agent with `message` (F5), as requested
    /// changes are.
    fn send_back(&self, message: &str, line: String) {
        let project = self.roster.project.id;
        self.host
            .changes_requested(project, self.thread, self.change, message);
        self.post(DutyEventKind::Changes, line);
    }

    /// Posts `line` unless it was the last thing said about this head.
    fn say_once(&self, kind: DutyEventKind, line: String) {
        let key = (self.roster.project.id, self.change.to_owned());
        {
            let mut state = self.runner.lock();
            let note = state
                .checking
                .entry(key)
                .or_insert_with(|| CheckNote::new(self.head));
            if note.said.as_deref() == Some(line.as_str()) {
                return;
            }
            note.said = Some(line.clone());
        }
        self.post(kind, line);
    }

    fn post(&self, kind: DutyEventKind, line: String) {
        self.host.update(
            self.roster.project.id,
            Box::new(move |_, timestamp| vec![DutyFeedEvent::new(timestamp, LANDER, kind, line)]),
        );
    }
}

impl CheckNote {
    fn new(head: &str) -> Self {
        Self {
            head: head.to_owned(),
            said: None,
            left_to_agent: false,
        }
    }
}

/// The Rosen change a candidate is, if it is one.
fn change_of(candidate: &ReviewCandidate) -> Option<&str> {
    match &candidate.subject {
        ReviewSubject::Change { change, .. } => Some(change),
        ReviewSubject::PullRequest(_) => None,
    }
}
