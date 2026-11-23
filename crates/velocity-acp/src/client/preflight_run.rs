//! Runs the `preflight` checks for a Rosen change (#537) and delivers one pass/fail report.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::{
    SessionUpdate, ToolCall, ToolCallId, ToolCallStatus, ToolKind,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use velocity_core::preflight::{
    ChangedFile, PreflightContext, PreflightPlan, StepOutcome, StepStatus, format_failure_summary,
    format_report, plan,
};
use velocity_core::queued_runs::{QueuedRunClass, QueuedRunPhase, QueuedRunRecord};
use velocity_core::rosen_checker::{
    BINDINGS, BINDINGS_EXPORTER, changes_bindings, regeneration_instructions,
};
use velocity_rosen::agent_run::{AgentRun, toolchain_path_from};
use velocity_rosen::landing::LandRefusal;
use velocity_rosen::light_target::{WorkspaceScope, scope_from_changed_paths};
use velocity_rosen::preflight::{
    PreflightSnapshot, RecordOutcome, discover_workspace_crates, inspect, record_outcome,
};
use velocity_rosen::queued_turn::{
    DEFAULT_WAIT_TIMEOUT, HELD_ENV, QUEUE_ENV, QueuedTurn, QueuedTurnError, QueuedTurnRequest,
    acquire,
};

use super::AgentLaunch;
use super::process::{hidden_command, spawn_owned_toolchain_command, stop_owned_process};
use super::program_path::{find_program_on_path, search_path};
use super::queued_run_command::RunResult;

const RUN_ID_PREFIX: &str = "velocity-preflight-";
const ROSEN_ENDPOINT: &str = "ROSEN_ENDPOINT";
const RUSTFMT_BATCH_SIZE: usize = 40;
const EXCERPT_DIAGNOSTICS: usize = 24;
const EXCERPT_TAIL: usize = 16;

struct Cancelled;

pub(super) struct PreflightRun {
    id: ToolCallId,
    pub log: PathBuf,
    endpoint: String,
    graph_context: Option<(PathBuf, PathBuf, String)>,
    queued_at: u64,
    queued_instant: Instant,
    output: File,
}

impl PreflightRun {
    /// Admits a preflight request, verifying this session has a Rosen workspace and opening
    /// its log file outside the workspace.
    pub fn admit(
        worktree: &Path,
        launch: &AgentLaunch,
        graph_context: Option<(PathBuf, PathBuf, String)>,
    ) -> Result<Self, String> {
        let endpoint = launch
            .env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(ROSEN_ENDPOINT))
            .map(|(_, value)| value.clone())
            .ok_or(
                "Preflight needs a Rosen workspace to inspect the change's diff, sensitive \
                 paths and overlaps, and this session has no Rosen server",
            )?;
        if !velocity_rosen::has_workspace(worktree) {
            return Err(format!(
                "Preflight needs a Rosen workspace, but {} has no .rosen marker",
                worktree.display()
            ));
        }
        let id = format!("{RUN_ID_PREFIX}{}", uuid::Uuid::new_v4());
        let log = std::env::temp_dir().join(format!("{id}.log"));
        let output = create_log(&log).map_err(|error| error.to_string())?;
        let queued_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Ok(Self {
            id: ToolCallId::new(id),
            log,
            endpoint,
            graph_context,
            queued_at,
            queued_instant: Instant::now(),
            output,
        })
    }

    pub fn started(&self) -> SessionUpdate {
        SessionUpdate::ToolCall(
            ToolCall::new(self.id.clone(), "preflight")
                .kind(ToolKind::Execute)
                .status(ToolCallStatus::InProgress),
        )
    }

    pub async fn finish(
        mut self,
        rosen: &Path,
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
    ) -> RunResult {
        if stop.is_cancelled() {
            return cancelled_result(self.id, &self.log);
        }
        let _ = writeln!(
            self.output,
            "Velocity preflight\nWorkspace: {}\n",
            worktree.display()
        );
        let snapshot = match inspect(&self.endpoint, worktree).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let message = format!("Could not inspect the Rosen change: {error}");
                let _ = writeln!(self.output, "{message}");
                return RunResult::new(
                    self.id,
                    false,
                    format!(
                        "[Velocity] Preflight failed · {message}\nFull log: {}",
                        self.log.display()
                    ),
                );
            }
        };
        let mut runner = StepRunner {
            rosen,
            worktree,
            launch,
            endpoint: &self.endpoint,
            graph_context: self.graph_context.as_ref(),
            clean_path: clean_toolchain_path(launch),
            log_path: &self.log,
            output: &mut self.output,
            stop: &stop,
            queue_wait_ms: 0,
        };
        let run_started = Instant::now();
        let initial_wait_ms =
            u64::try_from(self.queued_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
        match runner.run_all(&snapshot).await {
            Ok(steps) => {
                let total_wait_ms = initial_wait_ms.saturating_add(runner.queue_wait_ms);
                let total_elapsed_ms =
                    u64::try_from(run_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let run_ms = total_elapsed_ms.saturating_sub(runner.queue_wait_ms);
                let log_display = self.log.display().to_string();
                let (mut passed, mut report) =
                    format_report(&snapshot.change, snapshot.files.len(), &steps, &log_display);
                let failure_summary = if passed {
                    String::new()
                } else {
                    format_failure_summary(&steps, &log_display)
                };
                self.persist_attestation(
                    &snapshot,
                    &mut passed,
                    &mut report,
                    &failure_summary,
                    &log_display,
                )
                .await;
                let _ = writeln!(self.output, "\n=== summary ===\n{report}");
                let mut record = QueuedRunRecord::new(
                    self.id.0.to_string(),
                    "preflight".into(),
                    Vec::new(),
                    None,
                    QueuedRunClass::Verify,
                );
                record.log = Some(log_display);
                record.phase = if passed {
                    QueuedRunPhase::Completed
                } else {
                    QueuedRunPhase::Failed
                };
                record.change = Some(snapshot.change.clone());
                record.head = Some(snapshot.head.to_string());
                record.exit_code = Some(if passed { 0 } else { 1 });
                record.exit_status = Some(if passed {
                    "passed".into()
                } else {
                    "failed".into()
                });
                record.queued_at = Some(self.queued_at);
                record.wait_ms = total_wait_ms;
                record.run_ms = run_ms;
                RunResult::with_record(self.id, record, passed, report)
            }
            Err(Cancelled) => cancelled_result(self.id, &self.log),
        }
    }

    async fn persist_attestation(
        &mut self,
        snapshot: &PreflightSnapshot,
        passed: &mut bool,
        report: &mut String,
        failure_summary: &str,
        log_display: &str,
    ) {
        match record_outcome(snapshot, *passed, failure_summary, log_display).await {
            Ok(RecordOutcome::Recorded { receipt_submitted }) => {
                let _ = writeln!(
                    self.output,
                    "\n=== preflight record ===\nrecorded for head {} (passed: {}, rosen receipt: {receipt_submitted})",
                    snapshot.head, *passed
                );
            }
            Ok(RecordOutcome::StaleHead { checked, current }) => {
                let note = format!(
                    "Change moved while preflight was running (checked {}, current {}). Run `preflight` again before naming.",
                    checked.get(..8).unwrap_or(&checked),
                    current.get(..8).unwrap_or(&current),
                );
                let _ = writeln!(self.output, "\n=== preflight record ===\n{note}");
                if *passed {
                    *passed = false;
                    report.push_str(&format!("\n\n[Velocity] {note}"));
                }
            }
            Err(error) => {
                let head = snapshot.head.to_string();
                let note = format!(
                    "Could not record preflight result for snapshot {}: {error}. Run `preflight` again before naming.",
                    head.get(..8).unwrap_or(&head)
                );
                let _ = writeln!(self.output, "\n=== preflight record ===\n{note}");
                if *passed {
                    *passed = false;
                    report.push_str(&format!("\n\n[Velocity] {note}"));
                }
            }
        }
    }
}

struct StepRunner<'a> {
    rosen: &'a Path,
    worktree: &'a Path,
    launch: &'a AgentLaunch,
    endpoint: &'a str,
    graph_context: Option<&'a (PathBuf, PathBuf, String)>,
    clean_path: OsString,
    log_path: &'a Path,
    output: &'a mut File,
    stop: &'a CancellationToken,
    queue_wait_ms: u64,
}

impl StepRunner<'_> {
    async fn run_all(
        &mut self,
        snapshot: &PreflightSnapshot,
    ) -> Result<Vec<StepOutcome>, Cancelled> {
        let context = build_context(self.worktree);
        let changed = build_changed_files(self.worktree, snapshot);
        let check_plan = plan(&changed, &context);
        let mut steps = Vec::new();
        let blocking_skip = record_snapshot_checks(snapshot, &mut steps);
        steps.push(check_overlaps(snapshot, self.output).await);
        steps.push(self.check_signature_references(snapshot).await);
        if self.stop.is_cancelled() {
            return Err(Cancelled);
        }
        steps.push(self.check_rustfmt(&check_plan).await?);
        if let Some(reason) = blocking_skip {
            steps.extend(skipped_after_snapshot_failure(&check_plan, reason));
            return Ok(steps);
        }
        steps.extend(self.check_ui(&check_plan).await?);
        steps.extend(self.check_lockfiles(&check_plan).await?);
        steps.extend(
            self.check_rust_and_bindings(snapshot, &context, &check_plan)
                .await?,
        );
        Ok(steps)
    }

    async fn check_signature_references(&mut self, snapshot: &PreflightSnapshot) -> StepOutcome {
        let Some((executable, app_data, project)) = self.graph_context.cloned() else {
            return StepOutcome::skipped("signature references", "code graph unavailable");
        };
        let endpoint = self.endpoint.to_owned();
        let change = snapshot.change.clone();
        let warnings = tokio::task::spawn_blocking(move || {
            velocity_graph::change_signature_warnings(
                &executable,
                &app_data,
                &project,
                &endpoint,
                &change,
            )
        })
        .await
        .unwrap_or_default();
        if warnings.is_empty() {
            let _ = writeln!(self.output, "\n=== signature references ===\nnone");
            return StepOutcome::passed(
                "signature references",
                "no stale references in open or recent changes",
            );
        }
        let lines: Vec<String> = warnings
            .iter()
            .map(|warning| {
                format!(
                    "- {}",
                    velocity_graph::signature_impact::format_warning_line(warning)
                )
            })
            .collect();
        let detail = lines.join("\n");
        let _ = writeln!(self.output, "\n=== signature references ===\n{detail}");
        let summary = velocity_graph::signature_impact::format_warning_summary(&warnings);
        StepOutcome::warned("signature references", summary, Some(detail))
    }

    async fn check_rustfmt(
        &mut self,
        check_plan: &PreflightPlan,
    ) -> Result<StepOutcome, Cancelled> {
        if check_plan.rustfmt_by_edition.is_empty() {
            return Ok(StepOutcome::skipped(
                "rustfmt --check",
                "no changed Rust files",
            ));
        }
        let total: usize = check_plan.rustfmt_by_edition.values().map(Vec::len).sum();
        let rustfmt = find_program_on_path(OsStr::new("rustfmt"), &self.clean_path);
        let mut excerpts = Vec::new();
        let mut fix_hints = Vec::new();
        for (edition, files) in &check_plan.rustfmt_by_edition {
            for batch in files.chunks(RUSTFMT_BATCH_SIZE) {
                let mut arguments = vec![
                    String::from("--check"),
                    String::from("--edition"),
                    edition.clone(),
                ];
                arguments.extend(batch.iter().cloned());
                let step = self
                    .run_direct_command("rustfmt --check", &rustfmt, &arguments)
                    .await?;
                if let Some(excerpt) = step {
                    excerpts.push(excerpt);
                    fix_hints.push(format!("rustfmt --edition {edition} {}", batch.join(" ")));
                }
            }
        }
        if excerpts.is_empty() {
            let noun = if total == 1 { "file" } else { "files" };
            return Ok(StepOutcome::passed(
                "rustfmt --check",
                format!("{total} changed Rust {noun}"),
            ));
        }
        let mut detail = excerpts.join("\n\n");
        detail.push_str("\n\nFormat only changed files:\n");
        detail.push_str(&fix_hints.join("\n"));
        Ok(StepOutcome::failed(
            "rustfmt --check",
            "formatting differences in changed Rust files",
            Some(detail),
        ))
    }

    async fn check_ui(
        &mut self,
        check_plan: &PreflightPlan,
    ) -> Result<Vec<StepOutcome>, Cancelled> {
        if !check_plan.ui_check && !check_plan.ui_lint {
            return Ok(vec![StepOutcome::skipped("UI checks", "no ui/ changes")]);
        }
        let pnpm = find_program_on_path(OsStr::new("pnpm"), &self.clean_path);
        let mut outcomes = Vec::new();
        if check_plan.ui_check {
            let args = to_strings(&["-C", "ui", "check"]);
            outcomes.push(
                self.direct_outcome("pnpm -C ui check", &pnpm, &args, None)
                    .await?,
            );
        }
        if check_plan.ui_lint {
            let args = to_strings(&["-C", "ui", "lint"]);
            outcomes.push(
                self.direct_outcome(
                    "pnpm -C ui lint",
                    &pnpm,
                    &args,
                    Some("Run `pnpm -C ui format` if Prettier reported formatting issues."),
                )
                .await?,
            );
        }
        Ok(outcomes)
    }

    async fn check_lockfiles(
        &mut self,
        check_plan: &PreflightPlan,
    ) -> Result<Vec<StepOutcome>, Cancelled> {
        let mut outcomes = Vec::new();
        if check_plan.check_cargo_lock {
            let cargo = find_program_on_path(OsStr::new("cargo"), &self.clean_path);
            let args = to_strings(&["metadata", "--no-deps", "--format-version", "1", "--locked"]);
            outcomes.push(
                self.direct_outcome(
                    "Cargo.lock freshness",
                    &cargo,
                    &args,
                    Some(
                        "Regenerate `Cargo.lock`: `rosen run -- cargo metadata --format-version 1`",
                    ),
                )
                .await?,
            );
        }
        if check_plan.check_pnpm_lock {
            let pnpm = find_program_on_path(OsStr::new("pnpm"), &self.clean_path);
            let args = to_strings(&[
                "-C",
                "ui",
                "install",
                "--frozen-lockfile",
                "--lockfile-only",
            ]);
            outcomes.push(
                self.direct_outcome(
                    "ui/pnpm-lock.yaml freshness",
                    &pnpm,
                    &args,
                    Some("Regenerate `ui/pnpm-lock.yaml`: `pnpm -C ui install --lockfile-only`"),
                )
                .await?,
            );
        }
        Ok(outcomes)
    }

    async fn check_rust_and_bindings(
        &mut self,
        snapshot: &PreflightSnapshot,
        context: &PreflightContext,
        check_plan: &PreflightPlan,
    ) -> Result<Vec<StepOutcome>, Cancelled> {
        if !check_plan.needs_light_rust_turn() {
            return Ok(vec![StepOutcome::skipped(
                "Rust checks",
                "no touched Rust crates",
            )]);
        }
        let workspace_scope = light_target_scope(snapshot, context);
        let wait_started = Instant::now();
        let turn = match self.acquire_light_turn(workspace_scope).await {
            Ok(turn) => {
                let waited = u64::try_from(wait_started.elapsed().as_millis()).unwrap_or(u64::MAX);
                self.queue_wait_ms = self.queue_wait_ms.saturating_add(waited);
                turn
            }
            Err(QueuedTurnError::Cancelled) => return Err(Cancelled),
            Err(QueuedTurnError::TimedOut(report)) => {
                let _ = writeln!(
                    self.output,
                    "\n=== rust checks (queue timeout) ===\n{report}"
                );
                return Ok(vec![StepOutcome::failed(
                    "Rust checks",
                    "timed out waiting for a light Rust run-queue turn",
                    Some(report.to_string()),
                )]);
            }
            Err(QueuedTurnError::Io(error)) => {
                let _ = writeln!(self.output, "\n=== rust checks (queue error) ===\n{error}");
                return Ok(vec![StepOutcome::failed(
                    "Rust checks",
                    format!("could not acquire light Rust slot: {error}"),
                    None,
                )]);
            }
        };
        let target = match &turn {
            QueuedTurn::Light { target, .. } => target.clone(),
            _ => unreachable_target(self.worktree),
        };
        let outcomes = self.run_in_light_turn(check_plan, &target).await;
        drop(turn);
        outcomes
    }

    async fn run_in_light_turn(
        &mut self,
        check_plan: &PreflightPlan,
        target: &Path,
    ) -> Result<Vec<StepOutcome>, Cancelled> {
        let mut outcomes = Vec::new();
        let mut clippy_failed = false;
        if let Some((label, args)) = clippy_command(&check_plan.clippy_crates) {
            let outcome = self.light_outcome(&label, target, &args, None).await?;
            clippy_failed = outcome.status == StepStatus::Failed;
            outcomes.push(outcome);
        }
        if check_plan.check_bindings {
            outcomes.push(self.check_bindings(target, clippy_failed).await?);
        }
        Ok(outcomes)
    }

    async fn check_bindings(
        &mut self,
        target: &Path,
        clippy_failed: bool,
    ) -> Result<StepOutcome, Cancelled> {
        let label = "ui/src/lib/bindings.ts freshness";
        if clippy_failed {
            return Ok(StepOutcome::skipped(
                label,
                "skipped because cargo clippy failed",
            ));
        }
        // `export-bindings --check` overwrites bindings.ts before exiting non-zero; restore
        // the original bytes when done so preflight never mutates the workspace.
        let _guard = BindingsGuard::capture(self.worktree.join(BINDINGS));
        let args = to_strings(&[
            "run",
            "--manifest-path",
            "src-tauri/Cargo.toml",
            "--bin",
            "export-bindings",
            "--",
            "--check",
        ]);
        self.light_outcome(
            label,
            target,
            &args,
            Some("Regenerate `ui/src/lib/bindings.ts`: `rosen run -- pnpm -C ui bindings`"),
        )
        .await
    }

    async fn acquire_light_turn(
        &self,
        workspace_scope: Option<WorkspaceScope>,
    ) -> Result<QueuedTurn, QueuedTurnError> {
        let worktree = self.worktree.to_owned();
        let directory = self.worktree.to_owned();
        let queue_override = self.launch.env_value(QUEUE_ENV).map(PathBuf::from);
        let stop = self.stop.clone();
        tokio::task::spawn_blocking(move || {
            let request = QueuedTurnRequest {
                policy: AgentRun::RustCheck,
                class: QueuedRunClass::Build,
                worktree: &worktree,
                directory: &directory,
                command_line: "preflight rust checks",
                queue_override: queue_override.as_deref(),
                timeout: DEFAULT_WAIT_TIMEOUT,
                workspace_scope: workspace_scope.as_ref(),
            };
            acquire(&request, move || stop.is_cancelled())
        })
        .await
        .map_err(|error| QueuedTurnError::Io(io::Error::other(error)))?
    }

    async fn direct_outcome(
        &mut self,
        label: &str,
        program: &Path,
        arguments: &[String],
        hint: Option<&str>,
    ) -> Result<StepOutcome, Cancelled> {
        match self.run_direct_command(label, program, arguments).await? {
            None => Ok(StepOutcome::passed(label, "")),
            Some(mut excerpt) => {
                if let Some(hint) = hint {
                    excerpt.push_str(&format!("\n\n{hint}"));
                }
                Ok(StepOutcome::failed(label, "failed", Some(excerpt)))
            }
        }
    }

    async fn light_outcome(
        &mut self,
        label: &str,
        target: &Path,
        cargo_arguments: &[String],
        hint: Option<&str>,
    ) -> Result<StepOutcome, Cancelled> {
        let mut command = hidden_command(self.rosen);
        self.launch.apply_environment(&mut command);
        command
            .args(["run", "--class", "build", "--", "cargo"])
            .args(cargo_arguments)
            .env("PATH", &self.clean_path)
            .env(HELD_ENV, "build")
            .env("CARGO_BUILD_JOBS", "2")
            .env("CARGO_INCREMENTAL", "1")
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("CARGO_TARGET_DIR", target);
        match self.execute_command(label, command).await? {
            None => Ok(StepOutcome::passed(label, "")),
            Some(mut excerpt) => {
                if let Some(hint) = hint {
                    excerpt.push_str(&format!("\n\n{hint}"));
                }
                Ok(StepOutcome::failed(label, "failed", Some(excerpt)))
            }
        }
    }

    async fn run_direct_command(
        &mut self,
        label: &str,
        program: &Path,
        arguments: &[String],
    ) -> Result<Option<String>, Cancelled> {
        let mut command = hidden_command(program);
        self.launch.apply_environment(&mut command);
        command.args(arguments).env("PATH", &self.clean_path);
        self.execute_command(label, command).await
    }

    async fn execute_command(
        &mut self,
        label: &str,
        mut command: tokio::process::Command,
    ) -> Result<Option<String>, Cancelled> {
        if self.stop.is_cancelled() {
            return Err(Cancelled);
        }
        let _ = writeln!(self.output, "\n=== {label} ===");
        let _ = self.output.flush();
        let start = self.output.stream_position().unwrap_or(0);
        let (stdout, stderr) = match (self.output.try_clone(), self.output.try_clone()) {
            (Ok(stdout), Ok(stderr)) => (stdout, stderr),
            (Err(error), _) | (_, Err(error)) => return Ok(Some(format!("Log error: {error}"))),
        };
        command
            .current_dir(self.worktree)
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true);
        let mut child = match spawn_owned_toolchain_command(command) {
            Ok(child) => child,
            Err(error) => {
                let message = format!("Could not start `{label}`: {error}");
                let _ = writeln!(self.output, "{message}");
                return Ok(Some(message));
            }
        };
        let status = tokio::select! {
            result = child.wait() => result,
            () = self.stop.cancelled() => {
                let _ = stop_owned_process(&mut child).await;
                return Err(Cancelled);
            }
        };
        self.finish_step(label, status, start).await
    }

    async fn finish_step(
        &mut self,
        label: &str,
        status: io::Result<ExitStatus>,
        start: u64,
    ) -> Result<Option<String>, Cancelled> {
        let _ = self.output.flush();
        match status {
            Ok(exit) if exit.success() => Ok(None),
            Ok(exit) => {
                let path = self.log_path.to_owned();
                let excerpt = tokio::task::spawn_blocking(move || read_excerpt(&path, start))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| format!("`{label}` exited with {exit}"));
                Ok(Some(excerpt))
            }
            Err(error) => Ok(Some(format!("`{label}` failed: {error}"))),
        }
    }
}

fn record_snapshot_checks(
    snapshot: &PreflightSnapshot,
    steps: &mut Vec<StepOutcome>,
) -> Option<&'static str> {
    let workspace_tree = snapshot.workspace_tree.to_string();
    let change_head = snapshot.head.to_string();
    evaluate_snapshot_checks(
        &snapshot.unreadable,
        &workspace_tree,
        &change_head,
        snapshot.land_refusal.as_ref(),
        steps,
    )
}

/// Evaluates synchronous snapshot checks and returns a skip reason when a blocking
/// workspace/tree failure makes UI, lockfile, and queued Rust checks meaningless (#681).
fn evaluate_snapshot_checks(
    unreadable: &[String],
    workspace_tree: &str,
    change_head: &str,
    land_refusal: Option<&LandRefusal>,
    steps: &mut Vec<StepOutcome>,
) -> Option<&'static str> {
    let has_unreadable = !unreadable.is_empty();
    if has_unreadable {
        steps.push(StepOutcome::failed(
            "workspace snapshot",
            "some files are held open",
            Some(format!(
                "Close or save these files before naming the change: {}",
                unreadable.join(", ")
            )),
        ));
    }
    let behind_rebase = workspace_tree != change_head;
    if behind_rebase {
        steps.push(StepOutcome::failed(
            "workspace rebase",
            "workspace is behind a server rebase",
            Some(format!(
                "Workspace tree {} differs from change head {}. Run `rosen rebase` before running preflight.",
                workspace_tree.get(..8).unwrap_or(workspace_tree),
                change_head.get(..8).unwrap_or(change_head),
            )),
        ));
    }
    let has_conflict = match land_refusal {
        Some(LandRefusal::Regenerate { paths }) => {
            steps.push(StepOutcome::failed(
                "generated files (base merge)",
                format!("needs regeneration: {}", paths.join(", ")),
                Some(regeneration_instructions(paths).trim().to_owned()),
            ));
            false
        }
        Some(LandRefusal::Conflict { paths, message }) => {
            let detail = format!(
                "{message}\nRun `rosen rebase`, combine both sides, and run `rosen resolve <file>` for: {}",
                paths.join(", ")
            );
            steps.push(StepOutcome::failed(
                "merge conflicts",
                format!("{} path(s) conflict with base", paths.len()),
                Some(detail),
            ));
            true
        }
        Some(LandRefusal::Other { .. }) | None => false,
    };
    snapshot_skip_reason(has_unreadable, behind_rebase, has_conflict)
}

fn snapshot_skip_reason(
    has_unreadable: bool,
    behind_rebase: bool,
    has_conflict: bool,
) -> Option<&'static str> {
    match (has_unreadable, behind_rebase, has_conflict) {
        (false, false, false) => None,
        (true, false, false) => {
            Some("skipped because unreadable workspace files must be closed or saved first")
        }
        (false, true, false) => {
            Some("skipped because the workspace must rebase first (`rosen rebase`)")
        }
        (false, _, true) => {
            Some("skipped because the workspace must rebase and resolve merge conflicts first")
        }
        (true, _, _) => Some(
            "skipped because workspace snapshot, rebase, or merge conflict errors must be fixed first",
        ),
    }
}

fn skipped_after_snapshot_failure(check_plan: &PreflightPlan, reason: &str) -> Vec<StepOutcome> {
    let mut outcomes = Vec::new();
    if check_plan.ui_check || check_plan.ui_lint {
        outcomes.push(StepOutcome::skipped("UI checks", reason));
    } else {
        outcomes.push(StepOutcome::skipped("UI checks", "no ui/ changes"));
    }
    if check_plan.check_cargo_lock {
        outcomes.push(StepOutcome::skipped("Cargo.lock freshness", reason));
    }
    if check_plan.check_pnpm_lock {
        outcomes.push(StepOutcome::skipped("ui/pnpm-lock.yaml freshness", reason));
    }
    if check_plan.needs_light_rust_turn() {
        outcomes.push(StepOutcome::skipped("Rust checks", reason));
    } else {
        outcomes.push(StepOutcome::skipped(
            "Rust checks",
            "no touched Rust crates",
        ));
    }
    outcomes
}

async fn check_overlaps(snapshot: &PreflightSnapshot, output: &mut File) -> StepOutcome {
    if snapshot.overlaps.is_empty() {
        let _ = writeln!(output, "\n=== overlaps ===\nnone");
        return StepOutcome::passed("overlaps", "no active overlaps");
    }
    let mut lines = Vec::new();
    let mut shared_count = 0usize;
    for overlap in snapshot.overlaps.iter().take(8) {
        let symbols = velocity_graph::coordination::rosen_overlap_symbols(
            &snapshot.client,
            &snapshot.change,
            overlap,
        )
        .await;
        let (line, shared) = format_overlap_line(overlap, &symbols);
        shared_count += shared;
        lines.push(line);
    }
    let detail = lines.join("\n");
    let _ = writeln!(output, "\n=== overlaps ===\n{detail}");
    overlap_outcome(snapshot.overlaps.len(), shared_count, detail)
}

fn overlap_outcome(total: usize, shared_count: usize, detail: String) -> StepOutcome {
    // Open-change overlaps are advisory (#564): failing here deadlocks concurrent
    // changes symmetrically, while Rosen's base merge check already gates real conflicts.
    let summary = if shared_count > 0 {
        format!("{total} overlapping change(s), {shared_count} shared symbol(s)")
    } else {
        format!("{total} overlapping change(s), no shared symbols")
    };
    StepOutcome::warned("overlaps", summary, Some(detail))
}

fn format_overlap_line(
    overlap: &velocity_rosen::preflight::OverlapRecord,
    symbols: &serde_json::Value,
) -> (String, usize) {
    let change = overlap.change.to_string();
    let short = change.get(..8).unwrap_or(&change);
    let mut shared = Vec::new();
    let mut holder = Vec::new();
    if let Some(map) = symbols.as_object() {
        for (path, entry) in map {
            collect_symbol_names(path, entry, "shared", &mut shared);
            collect_symbol_names(path, entry, "holder", &mut holder);
        }
    }
    let line = if !shared.is_empty() {
        format!(
            "- {} ({short}) on {} [shared symbols: {}]",
            overlap.owner,
            overlap.paths.join(", "),
            shared.join(", ")
        )
    } else if !holder.is_empty() {
        format!(
            "- {} ({short}) on {} [holder edited: {}]",
            overlap.owner,
            overlap.paths.join(", "),
            holder.join(", ")
        )
    } else {
        format!(
            "- {} ({short}) on {}",
            overlap.owner,
            overlap.paths.join(", ")
        )
    };
    (line, shared.len())
}

fn collect_symbol_names(
    path: &str,
    entry: &serde_json::Value,
    key: &str,
    output: &mut Vec<String>,
) {
    if let Some(names) = entry.get(key).and_then(serde_json::Value::as_array) {
        for name in names.iter().filter_map(serde_json::Value::as_str) {
            output.push(format!("{path}::{name}"));
        }
    }
}

fn light_target_scope(
    snapshot: &PreflightSnapshot,
    context: &PreflightContext,
) -> Option<WorkspaceScope> {
    if !snapshot.unreadable.is_empty() || snapshot.workspace_tree != snapshot.diff_tree {
        return None;
    }
    let changed_paths: Vec<&str> = snapshot
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    scope_from_changed_paths(
        &snapshot.parent_tree.to_string(),
        &changed_paths,
        &context.crates,
    )
}

fn build_context(worktree: &Path) -> PreflightContext {
    let (crates, default_edition) = discover_workspace_crates(worktree);
    let (has_ui_check, has_ui_lint) = ui_scripts(worktree);
    PreflightContext {
        crates,
        default_edition,
        has_cargo_lock: worktree.join("Cargo.lock").is_file(),
        has_ui_check,
        has_ui_lint,
        has_pnpm_lock: worktree.join("ui").join("pnpm-lock.yaml").is_file(),
        has_bindings_exporter: worktree.join(BINDINGS_EXPORTER).is_file(),
    }
}

fn build_changed_files(worktree: &Path, snapshot: &PreflightSnapshot) -> Vec<ChangedFile> {
    snapshot
        .files
        .iter()
        .map(|file| {
            let full = worktree.join(&file.path);
            let exists = file.exists && full.is_file();
            let source = (exists && file.path.ends_with(".rs"))
                .then(|| std::fs::read_to_string(&full).ok())
                .flatten();
            let touches_bindings = changes_bindings(&file.path, source.as_deref());
            ChangedFile {
                path: file.path.clone(),
                exists,
                touches_bindings,
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct PackageJson {
    #[serde(default)]
    scripts: std::collections::BTreeMap<String, String>,
}

fn ui_scripts(worktree: &Path) -> (bool, bool) {
    let Ok(text) = std::fs::read_to_string(worktree.join("ui").join("package.json")) else {
        return (false, false);
    };
    let Ok(package) = serde_json::from_str::<PackageJson>(&text) else {
        return (false, false);
    };
    (
        package.scripts.contains_key("check"),
        package.scripts.contains_key("lint"),
    )
}

fn clean_toolchain_path(launch: &AgentLaunch) -> OsString {
    let existing = launch
        .env_value("PATH")
        .map(OsString::from)
        .or_else(search_path)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    toolchain_path_from(&existing).unwrap_or(existing)
}

fn unreachable_target(worktree: &Path) -> PathBuf {
    worktree.join("target").join("agent-check")
}

fn to_strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// Builds one `cargo clippy` command covering every touched crate so Cargo parses the
/// workspace and checks shared dependencies once per light Rust turn (#655).
fn clippy_command(crates: &[String]) -> Option<(String, Vec<String>)> {
    if crates.is_empty() {
        return None;
    }
    let mut args = Vec::with_capacity(crates.len() * 2 + 5);
    args.push(String::from("clippy"));
    let mut label = String::from("cargo clippy");
    for package in crates {
        args.push(String::from("-p"));
        args.push(package.clone());
        label.push_str(" -p ");
        label.push_str(package);
    }
    args.push(String::from("--all-targets"));
    args.push(String::from("--"));
    args.push(String::from("-D"));
    args.push(String::from("warnings"));
    label.push_str(" --all-targets");
    Some((label, args))
}

fn cancelled_result(id: ToolCallId, log: &Path) -> RunResult {
    RunResult::new(
        id,
        false,
        format!(
            "[Velocity] Preflight cancelled\nFull log: {}",
            log.display()
        ),
    )
}

/// Restores `ui/src/lib/bindings.ts` when `export-bindings --check` alters it on disk.
struct BindingsGuard {
    path: PathBuf,
    saved: Option<Vec<u8>>,
}

impl BindingsGuard {
    fn capture(path: PathBuf) -> Self {
        let saved = std::fs::read(&path).ok();
        Self { path, saved }
    }
}

impl Drop for BindingsGuard {
    fn drop(&mut self) {
        let current = std::fs::read(&self.path).ok();
        if current == self.saved {
            return;
        }
        match &self.saved {
            Some(bytes) => {
                let _ = std::fs::write(&self.path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn create_log(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.write(true).create_new(true).open(path)
}

fn read_excerpt(log: &Path, start: u64) -> io::Result<String> {
    let mut file = File::open(log)?;
    file.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut tail = VecDeque::new();
    while reader.by_ref().take(2048).read_until(b'\n', &mut bytes)? != 0 {
        if !bytes.ends_with(b"\n") {
            reader.skip_until(b'\n')?;
        }
        let line: String = String::from_utf8_lossy(&bytes)
            .trim_end()
            .chars()
            .take(500)
            .collect();
        if diagnostics.len() < EXCERPT_DIAGNOSTICS && is_diagnostic(&line) {
            diagnostics.push(line.clone());
        }
        if tail.len() == EXCERPT_TAIL {
            tail.pop_front();
        }
        tail.push_back(line);
        bytes.clear();
    }
    diagnostics.retain(|line| !tail.contains(line));
    Ok(diagnostics
        .into_iter()
        .chain(tail)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn is_diagnostic(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("error")
        || line.starts_with("Error")
        || line.starts_with("Diff in ")
        || line.starts_with("FAILED")
        || line.starts_with("---- ")
        || line.starts_with("-->")
        || line.contains(" ... FAILED")
        || line.contains(": error:")
        || line.contains("panicked at")
}

#[cfg(test)]
mod tests {
    use super::{
        clippy_command, evaluate_snapshot_checks, overlap_outcome, skipped_after_snapshot_failure,
    };
    use velocity_core::preflight::{PreflightPlan, StepOutcome, StepStatus, format_report};
    use velocity_rosen::landing::LandRefusal;

    // Risk (#564): failing preflight on open-change shared symbols deadlocks concurrent
    // changes symmetrically before either can land; overlaps must warn instead.
    #[test]
    fn open_change_overlaps_warn_even_with_shared_symbols() {
        let step = overlap_outcome(
            2,
            3,
            "- issue-102 (xzxzsktz) on design/screens/README.md [shared symbols: design/screens/README.md::Screens::row:Home]"
                .to_owned(),
        );
        assert_eq!(step.status, StepStatus::Warned);
        assert_eq!(step.summary, "2 overlapping change(s), 3 shared symbol(s)");
        let (passed, report) = format_report("qpxzxwup1234", 1, &[step], "preflight.log");
        assert!(passed);
        assert!(report.contains("[WARN] overlaps"));
    }

    // Risk (#655): running one cargo clippy process per touched crate multiplies Cargo
    // startup and fingerprint work while holding the per-project light-run lock.
    #[test]
    fn clippy_command_combines_touched_crates_into_one_invocation() {
        assert!(clippy_command(&[]).is_none());

        assert_eq!(
            clippy_command(&["velocity-acp".to_owned()]),
            Some((
                "cargo clippy -p velocity-acp --all-targets".to_owned(),
                vec![
                    "clippy".to_owned(),
                    "-p".to_owned(),
                    "velocity-acp".to_owned(),
                    "--all-targets".to_owned(),
                    "--".to_owned(),
                    "-D".to_owned(),
                    "warnings".to_owned(),
                ],
            ))
        );

        assert_eq!(
            clippy_command(&[
                "velocity-acp".to_owned(),
                "velocity-core".to_owned(),
                "velocity-rosen".to_owned(),
            ]),
            Some((
                "cargo clippy -p velocity-acp -p velocity-core -p velocity-rosen --all-targets"
                    .to_owned(),
                vec![
                    "clippy".to_owned(),
                    "-p".to_owned(),
                    "velocity-acp".to_owned(),
                    "-p".to_owned(),
                    "velocity-core".to_owned(),
                    "-p".to_owned(),
                    "velocity-rosen".to_owned(),
                    "--all-targets".to_owned(),
                    "--".to_owned(),
                    "-D".to_owned(),
                    "warnings".to_owned(),
                ],
            ))
        );
    }

    // Risk (#681): acquiring the shared light Rust queue lock and running UI, lockfile,
    // or clippy checks when workspace snapshot/rebase/conflict checks already failed wastes
    // queue wait time and invalidates shared target fingerprints on a stale/conflicted tree.
    #[test]
    fn blocking_snapshot_failures_short_circuit_heavy_checks() {
        let head = "1111111111111111111111111111111111111111";
        let stale = "2222222222222222222222222222222222222222";

        let mut clean_steps = Vec::new();
        assert!(evaluate_snapshot_checks(&[], head, head, None, &mut clean_steps).is_none());
        assert!(clean_steps.is_empty());

        let mut regenerate_steps = Vec::new();
        let regenerate = LandRefusal::Regenerate {
            paths: vec!["Cargo.lock".to_owned()],
        };
        assert!(
            evaluate_snapshot_checks(&[], head, head, Some(&regenerate), &mut regenerate_steps)
                .is_none()
        );
        assert_eq!(regenerate_steps.len(), 1);
        assert_eq!(regenerate_steps[0].status, StepStatus::Failed);

        let mut rebase_steps = Vec::new();
        let rebase_reason = evaluate_snapshot_checks(&[], stale, head, None, &mut rebase_steps);
        assert!(rebase_reason.is_some());
        assert_eq!(rebase_steps[0].name, "workspace rebase");
        assert_eq!(rebase_steps[0].status, StepStatus::Failed);

        let mut conflict_steps = Vec::new();
        let conflict = LandRefusal::Conflict {
            paths: vec!["crates/velocity-acp/src/lib.rs".to_owned()],
            message: "merge conflict".to_owned(),
        };
        let conflict_reason =
            evaluate_snapshot_checks(&[], head, head, Some(&conflict), &mut conflict_steps);
        assert!(conflict_reason.is_some());
        assert_eq!(conflict_steps[0].name, "merge conflicts");

        let mut unreadable_steps = Vec::new();
        let unreadable = vec!["crates/velocity-acp/src/lib.rs".to_owned()];
        let unreadable_reason =
            evaluate_snapshot_checks(&unreadable, head, head, None, &mut unreadable_steps);
        assert!(unreadable_reason.is_some());
        assert_eq!(unreadable_steps[0].name, "workspace snapshot");

        let full_plan = PreflightPlan {
            ui_check: true,
            ui_lint: true,
            clippy_crates: vec!["velocity-acp".to_owned()],
            check_cargo_lock: true,
            check_pnpm_lock: true,
            check_bindings: true,
            ..PreflightPlan::default()
        };
        let reason = rebase_reason.unwrap_or_default();
        let skipped = skipped_after_snapshot_failure(&full_plan, reason);
        assert_eq!(
            skipped,
            vec![
                StepOutcome::skipped("UI checks", reason),
                StepOutcome::skipped("Cargo.lock freshness", reason),
                StepOutcome::skipped("ui/pnpm-lock.yaml freshness", reason),
                StepOutcome::skipped("Rust checks", reason),
            ]
        );

        let mut all_steps = rebase_steps;
        all_steps.extend(skipped);
        let (passed, report) = format_report("ootnropm1234", 1, &all_steps, "preflight.log");
        assert!(!passed);
        assert!(report.contains("[FAIL] workspace rebase"));
        assert!(report.contains("[SKIP] Rust checks"));
    }

    // Risk (#681): when UI or Rust crates were not touched, short-circuited skip rows
    // should still reflect that those suites had no matching changes rather than implying
    // they would have run after rebase.
    #[test]
    fn skipped_after_snapshot_failure_keeps_untouched_summaries() {
        let skipped =
            skipped_after_snapshot_failure(&PreflightPlan::default(), "skipped because rebase");
        assert_eq!(
            skipped,
            vec![
                StepOutcome::skipped("UI checks", "no ui/ changes"),
                StepOutcome::skipped("Rust checks", "no touched Rust crates"),
            ]
        );
    }
}
