//! Post-land verification of a Rosen project (SPEC.md F7 "After a land"; Rosen R12 and §9
//! "Build queue"). Independent build and test passes batch the newest lands. In after-land
//! mode builds run ahead of background tests, with no check at land time. Rosen bisects
//! failures, reverts build breaks and retains test failures for the issue reporter. A
//! before-land project keeps its existing test-failure reverts and PR duty schedule.

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
use velocity_rosen::post_land::{self, Batch, Bisection, CheckKind};
use velocity_store::duty::PrDutyRoster;

use super::landing::LANDER;
use super::{PrDutyReviews, ReviewHost, check_feed};
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
    kind: CheckKind,
    legacy_tests: bool,
}

impl PrDutyReviews {
    /// Starts each verification track independently, off the tick. A long test run never
    /// prevents the build track from seeing and repairing a newer land.
    pub(super) fn verify_landed<Host: ReviewHost>(&self, host: &Host, roster: &PrDutyRoster) {
        // A project that switched to git still verifies what its Rosen threads landed.
        if !roster.project.rosen_in_use() {
            return;
        }
        let Ok(root) = Location::new(&roster.project, &roster.worktrees).project_root() else {
            return;
        };
        let project = roster.project.id;
        self.report_test_failures(host, roster);
        for kind in [CheckKind::Build, CheckKind::Test] {
            self.start_verification(host, roster, &root, project, kind);
        }
    }

    fn start_verification<Host: ReviewHost>(
        &self,
        host: &Host,
        roster: &PrDutyRoster,
        root: &std::path::Path,
        project: u32,
        kind: CheckKind,
    ) {
        let key = (project, kind.as_str());
        if !self.lock().verifying_projects.insert(key) {
            return;
        }
        let verify = Verify {
            runner: self.clone(),
            host: host.clone(),
            project,
            server: ProjectServer::new(&self.app_data, project),
            folder: root.join(if kind == CheckKind::Build {
                "post-land-builds"
            } else {
                VERIFY_FOLDER
            }),
            kind,
            legacy_tests: !roster.paused && !roster.agents.is_empty(),
        };
        tokio::spawn(async move {
            verify.run().await;
            verify.runner.lock().verifying_projects.remove(&key);
        });
    }
}

impl<Host: ReviewHost> Verify<Host> {
    async fn run(&self) {
        for _ in 0..BATCHES {
            let next = match self.kind {
                CheckKind::Build => post_land::next_build_batch(&self.server).await,
                CheckKind::Test => post_land::next_batch(&self.server).await,
            };
            let batch = match next {
                Ok(Some(batch)) => batch,
                // Every land is covered, or Rosen is not running: the next tick asks again.
                _ => return,
            };
            if self.kind == CheckKind::Test && !batch.after_land && !self.legacy_tests {
                return;
            }
            if batch.kind != self.kind || self.settled(&batch.commit) || !self.verify(&batch).await
            {
                return;
            }
        }
    }

    /// Tests one batch. `true` when it went green, so the next can be looked for.
    async fn verify(&self, batch: &Batch) -> bool {
        // A project with no post-land commands has nothing to fail: its lands are green.
        let started = Instant::now();
        let (failed, suffix) = if batch.commands.is_empty() {
            (None, String::new())
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
                    self.say(DutyEventKind::Approve, format!("{line}{suffix}"));
                    true
                }
                Err(error) => self.stopped(&batch.commit, &error.to_string()),
            };
        };
        self.bisect(batch, &failed, &suffix).await;
        false
    }

    /// Reports the failed batch and tests each commit Rosen asks for, until it names the
    /// change that broke the batch or finds that none does.
    async fn bisect(&self, batch: &Batch, failed: &FailedCheck, suffix: &str) {
        self.say(
            DutyEventKind::Changes,
            format!(
                "{}{suffix}",
                verify_failed_line(&batch.commit, &failed.command)
            ),
        );
        let report = failed_check_report(failed);
        let mut state = post_land::batch_failed(&self.server, batch, &report).await;
        for _ in 0..STEPS {
            let step = match state {
                Ok(Bisection::Step(step)) => step,
                Ok(Bisection::Culprit {
                    change,
                    commit,
                    reverted,
                    ..
                }) => {
                    if reverted {
                        self.reverted(&change, &commit);
                    } else {
                        self.say(DutyEventKind::Wait, format!("Post-land test failure in change {change} ({commit}) · recorded for issue reporting · nothing reverted"));
                        self.runner.wake();
                    }
                    return;
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
            let (passed, output, suffix) = match self.test(&step.commit, &batch.commands).await {
                Ok((None, suffix)) => (true, String::new(), suffix),
                Ok((Some(failed), suffix)) => (false, failed_check_report(&failed), suffix),
                // Rosen keeps the bisection: the next pass fails the batch again and
                // carries on from this step.
                Err(why) => {
                    self.stopped(&step.commit, &why);
                    return;
                }
            };
            self.step_feed(&step.commit, passed, &suffix);
            state = post_land::step_result(&self.server, &step, passed, &output).await;
        }
    }

    fn step_feed(&self, commit: &str, passed: bool, suffix: &str) {
        if self.kind != CheckKind::Build {
            return;
        }
        if !suffix.is_empty() {
            let verdict = if passed { "passed" } else { "failed" };
            let kind = if passed {
                DutyEventKind::Approve
            } else {
                DutyEventKind::Changes
            };
            self.say(
                kind,
                format!("Bisection check {verdict} on {commit}{suffix}"),
            );
        }
    }

    /// Brings the folder to `commit` and returns any failed command and optional telemetry.
    /// An error means the run itself could not happen, which is no verdict.
    async fn test(
        &self,
        commit: &str,
        commands: &[String],
    ) -> Result<(Option<FailedCheck>, String), String> {
        post_land::stage_commit(&self.server, commit, &self.folder)
            .await
            .map_err(|error| error.to_string())?;
        let (folder, commands) = (self.folder.clone(), commands.to_vec());
        // Build recovery goes ahead of the background full tests, so a broken base is
        // repaired promptly even while another project's long test suite is running.
        let priority = if self.kind == CheckKind::Build {
            Priority::Landing
        } else {
            Priority::Background
        };
        let ran = tokio::task::spawn_blocking(move || {
            checker::apply_staged(&folder)?;
            check_feed::run_commands(&folder, &commands, priority)
        })
        .await;
        match ran {
            Ok(Ok((failed, suffix))) => Ok((
                failed.map(|failed| FailedCheck {
                    command: failed.command,
                    exit_code: failed.exit_code,
                    output: failed.output,
                }),
                suffix,
            )),
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
        let note = state
            .verifying
            .entry((self.project, self.kind.as_str()))
            .or_default();
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
        let line = if self.kind == CheckKind::Build {
            format!("Build verification · {line}")
        } else {
            line
        };
        let new = {
            let mut state = self.runner.lock();
            let note = state
                .verifying
                .entry((self.project, self.kind.as_str()))
                .or_default();
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
