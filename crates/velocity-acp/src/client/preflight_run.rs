//! Runs the `preflight` checks for a Rosen change (#537) and delivers one pass/fail report.

use std::collections::{BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use agent_client_protocol::v1::schema::{
    SessionUpdate, ToolCall, ToolCallId, ToolCallStatus, ToolKind,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use velocity_core::preflight::{
    ChangedFile, PreflightContext, PreflightPlan, StepOutcome, format_report, plan,
};
use velocity_core::queued_runs::QueuedRunClass;
use velocity_core::rosen_checker::{
    BINDINGS, BINDINGS_EXPORTER, changes_bindings, regeneration_instructions,
};
use velocity_rosen::agent_run::{AgentRun, toolchain_path_from};
use velocity_rosen::landing::LandRefusal;
use velocity_rosen::preflight::{PreflightSnapshot, discover_workspace_crates, inspect};
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
    output: File,
}

impl PreflightRun {
    /// Admits a preflight request, verifying this session has a Rosen workspace and opening
    /// its log file outside the workspace.
    pub fn admit(worktree: &Path, launch: &AgentLaunch) -> Result<Self, String> {
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
        Ok(Self {
            id: ToolCallId::new(id),
            log,
            endpoint,
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
            clean_path: clean_toolchain_path(launch),
            log_path: &self.log,
            output: &mut self.output,
            stop: &stop,
        };
        match runner.run_all(&snapshot).await {
            Ok(steps) => {
                let log_display = self.log.display().to_string();
                let (passed, report) =
                    format_report(&snapshot.change, snapshot.files.len(), &steps, &log_display);
                let _ = writeln!(self.output, "\n=== summary ===\n{report}");
                RunResult::new(self.id, passed, report)
            }
            Err(Cancelled) => cancelled_result(self.id, &self.log),
        }
    }
}

struct StepRunner<'a> {
    rosen: &'a Path,
    worktree: &'a Path,
    launch: &'a AgentLaunch,
    clean_path: OsString,
    log_path: &'a Path,
    output: &'a mut File,
    stop: &'a CancellationToken,
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
        record_snapshot_checks(snapshot, &mut steps);
        steps.push(check_overlaps(snapshot, self.output).await);
        if self.stop.is_cancelled() {
            return Err(Cancelled);
        }
        steps.push(self.check_rustfmt(&check_plan).await?);
        steps.extend(self.check_ui(&check_plan).await?);
        steps.extend(self.check_lockfiles(&check_plan).await?);
        steps.extend(self.check_rust_and_bindings(&check_plan).await?);
        Ok(steps)
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
        check_plan: &PreflightPlan,
    ) -> Result<Vec<StepOutcome>, Cancelled> {
        if !check_plan.needs_light_rust_turn() {
            return Ok(vec![StepOutcome::skipped(
                "Rust checks",
                "no touched Rust crates",
            )]);
        }
        let turn = match self.acquire_light_turn().await {
            Ok(turn) => turn,
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
        let mut failed_crates = BTreeSet::new();
        for package in &check_plan.check_crates {
            let label = format!("cargo check -p {package}");
            let args = vec![String::from("check"), String::from("-p"), package.clone()];
            let outcome = self.light_outcome(&label, target, &args, None).await?;
            if outcome.status == velocity_core::preflight::StepStatus::Failed {
                failed_crates.insert(package.clone());
            }
            outcomes.push(outcome);
        }
        for package in &check_plan.clippy_crates {
            let label = format!("cargo clippy -p {package} (sensitive)");
            if failed_crates.contains(package) {
                outcomes.push(StepOutcome::skipped(
                    label,
                    "skipped because cargo check failed",
                ));
                continue;
            }
            let args = vec![
                String::from("clippy"),
                String::from("-p"),
                package.clone(),
                String::from("--all-targets"),
                String::from("--"),
                String::from("-D"),
                String::from("warnings"),
            ];
            outcomes.push(self.light_outcome(&label, target, &args, None).await?);
        }
        if check_plan.check_bindings {
            outcomes.push(
                self.check_bindings(check_plan, target, &failed_crates)
                    .await?,
            );
        }
        Ok(outcomes)
    }

    async fn check_bindings(
        &mut self,
        check_plan: &PreflightPlan,
        target: &Path,
        failed_crates: &BTreeSet<String>,
    ) -> Result<StepOutcome, Cancelled> {
        let label = "ui/src/lib/bindings.ts freshness";
        if check_plan
            .bindings_crate
            .as_ref()
            .is_some_and(|name| failed_crates.contains(name))
        {
            return Ok(StepOutcome::skipped(
                label,
                "skipped because the Tauri crate failed cargo check",
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

    async fn acquire_light_turn(&self) -> Result<QueuedTurn, QueuedTurnError> {
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

fn record_snapshot_checks(snapshot: &PreflightSnapshot, steps: &mut Vec<StepOutcome>) {
    if !snapshot.unreadable.is_empty() {
        steps.push(StepOutcome::failed(
            "workspace snapshot",
            "some files are held open",
            Some(format!(
                "Close or save these files before naming the change: {}",
                snapshot.unreadable.join(", ")
            )),
        ));
    }
    match &snapshot.land_refusal {
        Some(LandRefusal::Regenerate { paths }) => {
            steps.push(StepOutcome::failed(
                "generated files (base merge)",
                format!("needs regeneration: {}", paths.join(", ")),
                Some(regeneration_instructions(paths).trim().to_owned()),
            ));
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
        }
        Some(LandRefusal::Other { .. }) | None => {}
    }
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
    let total = snapshot.overlaps.len();
    if shared_count > 0 {
        StepOutcome::failed(
            "overlaps",
            format!("{total} overlapping change(s), {shared_count} shared symbol(s)"),
            Some(detail),
        )
    } else {
        StepOutcome::warned(
            "overlaps",
            format!("{total} overlapping change(s), no shared symbols"),
            Some(detail),
        )
    }
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
            let sensitive = snapshot
                .sensitive
                .iter()
                .any(|pattern| pattern.matches(&file.path));
            let touches_bindings = changes_bindings(&file.path, source.as_deref());
            ChangedFile {
                path: file.path.clone(),
                exists,
                sensitive,
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
