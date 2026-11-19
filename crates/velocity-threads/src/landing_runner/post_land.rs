//! Executes Rosen’s post-land verification jobs. Rosen owns bisection, reverts
//! and durable failure records (F7, R12).

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

use velocity_core::duty::DutyEventKind;
use velocity_core::rosen_checker::{
    FailedCheck, bisecting_line, failed_check_report, reverted_line, unconfirmed_line,
    verified_line, verify_failed_line, verify_stopped_line, verifying_line,
};
use velocity_rosen::ProjectServer;
use velocity_rosen::post_land::{self, Batch, Bisection, CheckKind};
use velocity_rosen::{checker, integration};
use velocity_store::duty::LandingProject;

use super::LANDER;
use super::{LandingHost, RosenLanding, check_feed};
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
    runner: RosenLanding,
    host: Host,
    project: u32,
    server: ProjectServer,
    folder: PathBuf,
    kind: CheckKind,
}

impl RosenLanding {
    /// Verification and integration run off the tick so dispatch can keep going.
    pub(super) fn verify_landed<Host: LandingHost>(
        &self,
        host: &Host,
        project_state: &LandingProject,
    ) {
        // A project that switched to git still verifies what its Rosen threads landed.
        if !project_state.project.rosen_in_use() {
            return;
        }
        let Ok(root) =
            Location::new(&project_state.project, &project_state.worktrees).project_root()
        else {
            return;
        };
        let project = project_state.project.id;
        self.report_test_failures(host, project_state);
        for kind in [CheckKind::Build, CheckKind::Test] {
            self.start_verification(host, &root, project, kind);
        }
    }

    fn start_verification<Host: LandingHost>(
        &self,
        host: &Host,
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
        };
        tokio::spawn(async move {
            verify.run().await;
            verify.runner.lock().verifying_projects.remove(&key);
        });
    }
}

impl<Host: LandingHost> Verify<Host> {
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
            // Rosen may offer a merge-mode build here; the independent build worker
            // owns its folder and advances that prefix before tests are offered.
            if batch.kind != self.kind || self.settled(&batch.commit) {
                return;
            }
            if batch.after_land && batch.kind == CheckKind::Test {
                self.integrate(&batch).await;
                return;
            }
            if !self.verify(&batch).await {
                return;
            }
        }
    }

    async fn integrate(&self, batch: &Batch) {
        let idle = self.host.integration_idle(self.project);
        match integration::due(&self.server, &batch.commit, idle) {
            Ok(true) => {}
            Ok(false) => return,
            Err(error) => {
                self.stopped(&batch.commit, &error.to_string());
                return;
            }
        }
        let _slot = match integration::try_slot() {
            Ok(Some(slot)) => slot,
            Ok(None) => return,
            Err(error) => {
                self.stopped(&batch.commit, &error.to_string());
                return;
            }
        };
        self.verify(batch).await;
    }

    /// Tests one batch. `true` when it went green, so the next can be looked for.
    async fn verify(&self, batch: &Batch) -> bool {
        // A project with no post-land commands has nothing to fail: its lands are green.
        let started = Instant::now();
        let is_integration = batch.after_land && batch.kind == CheckKind::Test;
        let already_passed = is_integration
            && match integration::passed(&self.server, &batch.commit) {
                Ok(passed) => passed,
                Err(error) => return self.stopped(&batch.commit, &error.to_string()),
            };
        let (failed, suffix) = if batch.commands.is_empty() || already_passed {
            (None, String::new())
        } else {
            let line = verifying_line(&batch.commit);
            self.say(
                DutyEventKind::Wait,
                if is_integration {
                    format!("Integration build · {line}")
                } else {
                    line
                },
            );
            match self.test(&batch.commit, batch).await {
                Ok(failed) => failed,
                Err(why) => return self.stopped(&batch.commit, &why),
            }
        };
        let Some(failed) = failed else {
            if is_integration
                && !batch.commands.is_empty()
                && let Err(error) = integration::result(&self.server, &batch.commit, true)
            {
                return self.stopped(&batch.commit, &error.to_string());
            }
            let result = post_land::batch_passed(&self.server, batch).await;
            if result.is_ok() && (is_integration || batch.kind == CheckKind::Build) {
                self.runner.wake();
            }
            return match result {
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
        let report = failure_report(batch, failed);
        let mut state = post_land::batch_failed(&self.server, batch, &report).await;
        for _ in 0..STEPS {
            let step = match state {
                Ok(Bisection::Step(step)) => step,
                Ok(Bisection::Culprit {
                    change,
                    commit,
                    reverted,
                    kind,
                }) => {
                    self.record_failed_integration(batch, failed);
                    if batch.after_land && kind == CheckKind::Test && !reverted {
                        self.say(DutyEventKind::Changes, format!(
                            "Integration build failed at {commit} in change {change} · fix forward"
                        ));
                        self.runner.wake();
                    } else {
                        self.culprit(&change, &commit, kind, reverted);
                    }
                    return;
                }
                Ok(Bisection::Unconfirmed) => {
                    self.record_failed_integration(batch, failed);
                    return self.settle(&batch.commit, unconfirmed_line(&batch.commit));
                }
                Err(error) => {
                    let line = verify_stopped_line(&batch.commit, &error.to_string());
                    return self.settle(&batch.commit, line);
                }
            };
            self.say(DutyEventKind::Wait, bisecting_line(&step.commit));
            // Keep the failed job's immutable check set and priority: a build
            // bisection must never substitute the project's full-test commands.
            let (passed, output, suffix) = match self.test(&step.commit, batch).await {
                Ok((None, suffix)) => (true, String::new(), suffix),
                Ok((Some(failed), suffix)) => (false, failure_report(batch, &failed), suffix),
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

    fn record_failed_integration(&self, batch: &Batch, failed: &FailedCheck) {
        if !batch.after_land || batch.kind != CheckKind::Test {
            return;
        }
        let exit = failed
            .exit_code
            .map_or_else(|| "timed out".to_owned(), |code| format!("exited {code}"));
        let reason = format!("`{}` {exit}", failed.command);
        if let Err(error) = integration::record_failure(&self.server, &batch.commit, &reason)
            .and_then(|()| integration::result(&self.server, &batch.commit, false))
        {
            self.stopped(&batch.commit, &error.to_string());
        }
    }

    fn culprit(&self, change: &str, commit: &str, kind: CheckKind, reverted: bool) {
        if reverted {
            self.reverted(change, commit);
        } else if kind == CheckKind::Test {
            self.say(DutyEventKind::Wait, format!("Post-land test failure in change {change} ({commit}) · recorded for issue reporting · nothing reverted"));
            self.runner.wake();
        } else {
            self.say(DutyEventKind::Changes, format!("Build culprit {commit} in change {change} remains on the base · fix-forward scheduling will inspect Rosen's revert evidence"));
            self.runner.wake();
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
        batch: &Batch,
    ) -> Result<(Option<FailedCheck>, String), String> {
        post_land::stage_commit(&self.server, commit, &self.folder)
            .await
            .map_err(|error| error.to_string())?;
        let (folder, commands) = (self.folder.clone(), batch.commands.clone());
        let priority = batch.priority.into();
        let kind = batch.kind;
        let integration = batch.after_land && kind == CheckKind::Test;
        let ran = tokio::task::spawn_blocking(move || {
            checker::apply_staged(&folder)?;
            if kind == CheckKind::Build {
                check_feed::run_commands(&folder, &commands, priority)
            } else if integration {
                checker::run_integration_commands(&folder, &commands)
                    .map(|failed| (failed, String::new()))
            } else {
                checker::run_commands(&folder, &commands, priority)
                    .map(|failed| (failed, String::new()))
            }
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
            self.host.activity(self.project, LANDER, kind, line);
        }
    }
}

fn failure_report(batch: &Batch, failed: &FailedCheck) -> String {
    let report = failed_check_report(failed);
    if batch.after_land && batch.kind == CheckKind::Test {
        format!("{}{report}", integration::FAILURE_HEADING)
    } else {
        report
    }
}
