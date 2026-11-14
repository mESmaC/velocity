//! Post-land verification of a Rosen project (SPEC.md F7 "After a land"; Rosen R12 and §9
//! "Build queue"). The pre-land checks are the fast ones, so a land never waits on the full
//! tests. Velocity runs those afterwards, on the newest landed commit: one passing run covers
//! every land up to it. When a run fails, Rosen names the commits to test until one change
//! fails twice where the commit before it passes, then reverts that change and reopens it.
//!
//! One pass runs per project, in its own folder that keeps its build output, at below-normal
//! priority: it shares the machine with the pre-land checks and the agents' builds, and none
//! of them waits on it.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use velocity_core::duty::{DutyEventKind, DutyFeedEvent};
use velocity_core::rosen_checker::{
    FailedCheck, bisecting_line, failed_check_report, reverted_line, unconfirmed_line,
    verified_line, verify_failed_line, verify_stopped_line, verifying_line,
};
use velocity_rosen::ProjectServer;
use velocity_rosen::checker::{self, Priority};
use velocity_rosen::post_land::{self, Batch, Bisection};
use velocity_store::duty::PrDutyRoster;

use super::landing::LANDER;
use super::{PrDutyReviews, ReviewHost};
use crate::launch::Location;

/// How many batches one pass tests before the next tick starts another. Lands that arrive
/// during a run make the next batch, so a pass only loops while changes keep landing.
const BATCHES: u32 = 8;
/// The most steps one bisection is walked in a pass. Rosen halves the interval each step,
/// then confirms twice, so no real interval comes near it.
const STEPS: u32 = 64;
/// The project's folder the post-land checks build in, beside its agents' workspaces.
const VERIFY_FOLDER: &str = "post-land-checks";

/// What Velocity has said about the batch it last tested in one project.
#[derive(Default)]
pub(super) struct VerifyNote {
    /// The batch's commit: a newer batch starts afresh.
    commit: String,
    /// The feed lines posted about it, so a pass that tests it again says nothing twice.
    said: BTreeSet<String>,
    /// Nothing more can be done for this batch until another change lands: no one change
    /// explains its failure, or Rosen refused a result.
    settled: bool,
}

/// One project's pass.
struct Verify<Host> {
    runner: PrDutyReviews,
    host: Host,
    project: u32,
    server: ProjectServer,
    folder: PathBuf,
}

impl PrDutyReviews {
    /// Starts testing what landed on a Rosen project since its last green commit, unless a
    /// pass already runs for it. Off the tick: a run takes as long as the project's tests.
    pub(super) fn verify_landed<Host: ReviewHost>(&self, host: &Host, roster: &PrDutyRoster) {
        if roster.project.version_control != velocity_store::VersionControl::Rosen {
            return;
        }
        let Ok(root) = Location::new(&roster.project, &roster.worktrees).project_root() else {
            return;
        };
        let project = roster.project.id;
        if !self.lock().verifying_projects.insert(project) {
            return;
        }
        let verify = Verify {
            runner: self.clone(),
            host: host.clone(),
            project,
            server: ProjectServer::new(&self.app_data, project),
            folder: root.join(VERIFY_FOLDER),
        };
        tokio::spawn(async move {
            verify.run().await;
            verify.runner.lock().verifying_projects.remove(&project);
        });
    }
}

impl<Host: ReviewHost> Verify<Host> {
    async fn run(&self) {
        for _ in 0..BATCHES {
            let batch = match post_land::next_batch(&self.server).await {
                Ok(Some(batch)) => batch,
                // Every land is covered, or Rosen is not running: the next tick asks again.
                _ => return,
            };
            if self.settled(&batch.commit) || !self.verify(&batch).await {
                return;
            }
        }
    }

    /// Tests one batch. `true` when it went green, so the next can be looked for.
    async fn verify(&self, batch: &Batch) -> bool {
        // A project with no post-land commands has nothing to fail: its lands are green.
        let started = Instant::now();
        let failed = if batch.commands.is_empty() {
            None
        } else {
            self.say(DutyEventKind::Wait, verifying_line(&batch.commit));
            match self.test(&batch.commit, &batch.commands).await {
                Ok(failed) => failed,
                Err(why) => return self.stopped(&batch.commit, &why),
            }
        };
        let Some(failed) = failed else {
            return match post_land::batch_passed(&self.server, batch).await {
                Ok(()) if batch.commands.is_empty() => true,
                Ok(()) => {
                    let line = verified_line(&batch.commit, started.elapsed());
                    self.say(DutyEventKind::Approve, line);
                    true
                }
                Err(error) => self.stopped(&batch.commit, &error.to_string()),
            };
        };
        self.bisect(batch, &failed).await;
        false
    }

    /// Reports the failed batch and tests each commit Rosen asks for, until it names the
    /// change that broke the batch or finds that none does.
    async fn bisect(&self, batch: &Batch, failed: &FailedCheck) {
        self.say(
            DutyEventKind::Changes,
            verify_failed_line(&batch.commit, &failed.command),
        );
        let report = failed_check_report(failed);
        let mut state = post_land::batch_failed(&self.server, batch, &report).await;
        for _ in 0..STEPS {
            let step = match state {
                Ok(Bisection::Step(step)) => step,
                Ok(Bisection::Culprit { change, commit }) => {
                    return self.reverted(&change, &commit);
                }
                Ok(Bisection::Unconfirmed) => {
                    return self.settle(&batch.commit, unconfirmed_line(&batch.commit));
                }
                Err(error) => {
                    let line = verify_stopped_line(&batch.commit, &error.to_string());
                    return self.settle(&batch.commit, line);
                }
            };
            self.say(DutyEventKind::Wait, bisecting_line(&step.commit));
            let (passed, output) = match self.test(&step.commit, &batch.commands).await {
                Ok(None) => (true, String::new()),
                Ok(Some(failed)) => (false, failed_check_report(&failed)),
                // Rosen keeps the bisection: the next pass fails the batch again and
                // carries on from this step.
                Err(why) => {
                    self.stopped(&step.commit, &why);
                    return;
                }
            };
            state = post_land::step_result(&self.server, &step, passed, &output).await;
        }
    }

    /// Brings the folder to `commit` and runs `commands` there. `Ok(None)` when every
    /// command passed; an error when the run itself could not happen, which is no verdict.
    async fn test(&self, commit: &str, commands: &[String]) -> Result<Option<FailedCheck>, String> {
        post_land::stage_commit(&self.server, commit, &self.folder)
            .await
            .map_err(|error| error.to_string())?;
        let (folder, commands) = (self.folder.clone(), commands.to_vec());
        let ran = tokio::task::spawn_blocking(move || {
            checker::apply_staged(&folder)?;
            checker::run_commands(&folder, &commands, Priority::Background)
        })
        .await;
        match ran {
            Ok(Ok(failed)) => Ok(failed.map(|failed| FailedCheck {
                command: failed.command,
                exit_code: failed.exit_code,
                output: failed.output,
            })),
            Ok(Err(error)) => Err(error.to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Rosen landed the revert and reopened the change: the Rosen event gives the change
    /// back to its thread's agent (F6), and the revert is a land the next pass tests.
    fn reverted(&self, change: &str, commit: &str) {
        self.say(DutyEventKind::Changes, reverted_line(change, commit));
        self.runner.wake();
    }

    /// The checks could not run or report, which is no verdict on any change: the feed says
    /// why once, and the next tick tries again.
    fn stopped(&self, commit: &str, why: &str) -> bool {
        self.say(DutyEventKind::Wait, verify_stopped_line(commit, why));
        false
    }

    /// Leaves `batch` alone until another change lands, and says why.
    fn settle(&self, batch: &str, line: String) {
        self.note(batch, |note| note.settled = true);
        self.say(DutyEventKind::Wait, line);
    }

    fn settled(&self, batch: &str) -> bool {
        self.note(batch, |note| note.settled)
    }

    /// Reads or changes the note on `batch`, starting a new one when the batch is new.
    fn note<T>(&self, batch: &str, change: impl FnOnce(&mut VerifyNote) -> T) -> T {
        let mut state = self.runner.lock();
        let note = state.verifying.entry(self.project).or_default();
        if note.commit != batch {
            *note = VerifyNote {
                commit: batch.to_owned(),
                ..VerifyNote::default()
            };
        }
        change(note)
    }

    /// Posts `line` unless it was already said about the batch being tested.
    fn say(&self, kind: DutyEventKind, line: String) {
        let new = {
            let mut state = self.runner.lock();
            let note = state.verifying.entry(self.project).or_default();
            note.said.insert(line.clone())
        };
        if new {
            self.host.update(
                self.project,
                Box::new(move |_, timestamp| {
                    vec![DutyFeedEvent::new(timestamp, LANDER, kind, line)]
                }),
            );
        }
    }
}
