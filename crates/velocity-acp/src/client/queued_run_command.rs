//! Run the command behind Rosen and keep the complete output outside the workspace.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, Meta, SessionUpdate, ToolCall, ToolCallContent, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use tokio_util::sync::CancellationToken;
use velocity_core::queued_runs::{QueuedRunPhase, QueuedRunRecord};
use velocity_rosen::agent_run::{AgentRun, BLOCK_VCS_ENV, toolchain_path_from};
use velocity_rosen::queued_turn::{
    DEFAULT_WAIT_TIMEOUT, HELD_ENV, QUEUE_ENV, QueueTimeoutReport, QueuedTurn, QueuedTurnError,
    QueuedTurnRequest, THREAD_VARIABLES, acquire_with_ticket, workspace_head,
};
use velocity_rosen::registered_generator::{
    GeneratorExecution, GeneratorGuard, RegisteredGenerator, begin_guard, finish_guard,
};

use super::AgentLaunch;
use super::process::{hidden_command, spawn_owned_toolchain_command, stop_owned_process};
use super::program_path::{find_program_on_path, search_path};
use super::queued_run_tool::RunCommand;
use super::queued_runs::RunStatusSink;

const RUN_ID_PREFIX: &str = "velocity-run-";
const RUN_NOTICE: &str = "velocity.queued_run_result";
const STATUS_EXCERPT_BYTES: usize = 4096;

pub(super) struct QueuedRun {
    id: ToolCallId,
    title: String,
    pub log: PathBuf,
    command: RunCommand,
    record: QueuedRunRecord,
    queued_instant: Instant,
    output: File,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

enum ExecutionOutcome {
    Exited(ExitStatus),
    TimedOut(Box<QueueTimeoutReport>),
    Cancelled,
    Failed(io::Error),
}

impl QueuedRun {
    pub fn new(command: RunCommand, worktree: &Path, launch: &AgentLaunch) -> io::Result<Self> {
        let id = format!("{RUN_ID_PREFIX}{}", uuid::Uuid::new_v4());
        let log = std::env::temp_dir().join(format!("{id}.log"));
        let (change, head) = workspace_context(worktree, launch);
        let mut record = command.to_record(id.clone());
        record.log = Some(log.display().to_string());
        record.phase = QueuedRunPhase::WaitingForTurnEnd;
        record.queue_reason = Some("waiting for the current agent turn to end".into());
        record.change = change;
        record.head = head;
        record.queued_at = Some(unix_now());
        Self::with_record(record, log, command)
    }

    /// Reconstructs a persisted run after a Velocity restart, preserving its durable ID
    /// while giving the restarted attempt a fresh log file.
    pub fn restored(
        previous: &QueuedRunRecord,
        worktree: &Path,
        launch: &AgentLaunch,
    ) -> io::Result<Self> {
        let log = std::env::temp_dir().join(format!(
            "{}-restored-{}.log",
            previous.id,
            uuid::Uuid::new_v4()
        ));
        let command = RunCommand::from_record(previous);
        let (change, head) = workspace_context(worktree, launch);
        let mut record = command.to_record(previous.id.clone());
        record.log = Some(log.display().to_string());
        record.phase = QueuedRunPhase::Queued;
        record.queue_reason = Some("requeued after Velocity restart".into());
        record.change = change.or_else(|| previous.change.clone());
        record.head = head.or_else(|| previous.head.clone());
        record.queued_at = previous.queued_at.or_else(|| Some(unix_now()));
        Self::with_record(record, log, command)
    }

    fn with_record(record: QueuedRunRecord, log: PathBuf, command: RunCommand) -> io::Result<Self> {
        let output = create_log(&log)?;
        let title = record.title();
        let id = ToolCallId::new(record.id.clone());
        Ok(Self {
            id,
            title,
            log,
            command,
            record,
            queued_instant: Instant::now(),
            output,
        })
    }

    pub fn record(&self) -> QueuedRunRecord {
        self.record.clone()
    }

    pub fn started(&self) -> SessionUpdate {
        SessionUpdate::ToolCall(
            ToolCall::new(self.id.clone(), self.title.clone())
                .kind(ToolKind::Execute)
                .status(ToolCallStatus::InProgress),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish(
        mut self,
        rosen: &Path,
        policy: AgentRun,
        registered: Option<RegisteredGenerator>,
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
        sink: RunStatusSink,
    ) -> RunResult {
        let outcome = self
            .run(rosen, policy, registered, worktree, launch, stop, &sink)
            .await;
        let cancelled_by_user = sink.is_cancelled_by_user();
        let outcome = if cancelled_by_user {
            ExecutionOutcome::Cancelled
        } else {
            outcome
        };
        let formatted = format_outcome(&self.record, &self.title, &self.log, outcome);
        let mut result = formatted.message;
        if formatted.include_log {
            let log = self.log.clone();
            match tokio::task::spawn_blocking(move || failure_output(&log)).await {
                Ok(Ok(output)) if !output.is_empty() => {
                    result.push_str("\n\n");
                    result.push_str(&output);
                }
                Ok(Err(error)) => result.push_str(&format!("\nCould not read the log: {error}")),
                _ => {}
            }
        }
        let result = velocity_core::text_budget::prompt_excerpt(
            &result,
            velocity_core::text_budget::PROMPT_BYTES,
            "\n[Output excerpt. Read the full log at the path above.]",
        );
        self.record.phase = formatted.phase;
        self.record.queue_reason = None;
        self.record.exit_code = formatted.exit_code;
        self.record.exit_status = Some(formatted.exit_status);
        self.record.result = Some(result.clone());
        self.record.delivered = cancelled_by_user;
        sink.finish(self.record.clone());
        RunResult {
            id: self.id,
            record: Some(self.record),
            succeeded: formatted.succeeded,
            result,
            deliver_prompt: !cancelled_by_user,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        &mut self,
        rosen: &Path,
        policy: AgentRun,
        registered: Option<RegisteredGenerator>,
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
        sink: &RunStatusSink,
    ) -> ExecutionOutcome {
        if stop.is_cancelled() {
            return ExecutionOutcome::Cancelled;
        }
        let directory = resolve_directory(worktree, self.command.directory.as_deref());
        if !directory.is_dir() {
            return ExecutionOutcome::Failed(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Working directory does not exist: {}", directory.display()),
            ));
        }
        if let Err(error) = writeln!(
            &self.output,
            "{}\nRun ID: {}\nDirectory: {}\n",
            self.title,
            self.record.id,
            directory.display()
        ) {
            return ExecutionOutcome::Failed(error);
        }
        sink.set_queued(Some(format!(
            "waiting for a `{}` run-queue turn",
            self.command.class()
        )));
        let turn = match self
            .acquire_turn(policy, worktree, &directory, launch, &stop, sink)
            .await
        {
            Ok(turn) => turn,
            Err(QueuedTurnError::Cancelled) => {
                self.record.wait_ms =
                    u64::try_from(self.queued_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
                return ExecutionOutcome::Cancelled;
            }
            Err(QueuedTurnError::TimedOut(report)) => {
                self.record.wait_ms =
                    u64::try_from(self.queued_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
                let _ = writeln!(&self.output, "{report}");
                return ExecutionOutcome::TimedOut(report);
            }
            Err(QueuedTurnError::Io(error)) => {
                self.record.wait_ms =
                    u64::try_from(self.queued_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
                return ExecutionOutcome::Failed(error);
            }
        };
        self.record.wait_ms =
            u64::try_from(self.queued_instant.elapsed().as_millis()).unwrap_or(u64::MAX);
        let run_started = Instant::now();
        let (change, head) = workspace_context(worktree, launch);
        if change.is_some() {
            self.record.change = change;
        }
        if head.is_some() {
            self.record.head = head;
        }
        sink.set_running(self.record.change.clone(), self.record.head.clone());
        let outcome = self
            .execute_under_turn(
                rosen,
                registered.as_ref(),
                worktree,
                &directory,
                launch,
                &turn,
                &stop,
            )
            .await;
        self.record.run_ms = u64::try_from(run_started.elapsed().as_millis()).unwrap_or(u64::MAX);
        drop(turn);
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_under_turn(
        &mut self,
        rosen: &Path,
        registered: Option<&RegisteredGenerator>,
        worktree: &Path,
        directory: &Path,
        launch: &AgentLaunch,
        turn: &QueuedTurn,
        stop: &CancellationToken,
    ) -> ExecutionOutcome {
        let guard = match self
            .begin_registered_guard(registered, worktree, launch)
            .await
        {
            Ok(guard) => guard,
            Err(error) => return ExecutionOutcome::Failed(error),
        };
        if let Some(outcome) = self
            .prepare_lake_cache(rosen, worktree, directory, launch, turn, stop)
            .await
        {
            return self.finalize_registered_guard(guard, outcome).await;
        }
        let command = match self.build_command(rosen, directory, launch, turn, registered) {
            Ok(command) => command,
            Err(error) => {
                return self
                    .finalize_registered_guard(guard, ExecutionOutcome::Failed(error))
                    .await;
            }
        };
        let mut child = match self.spawn_command(command) {
            Ok(child) => child,
            Err(error) => {
                return self
                    .finalize_registered_guard(guard, ExecutionOutcome::Failed(error))
                    .await;
            }
        };
        let outcome = tokio::select! {
            status = child.wait() => status.map_or_else(ExecutionOutcome::Failed, ExecutionOutcome::Exited),
            () = stop.cancelled() => {
                let _ = stop_owned_process(&mut child).await;
                ExecutionOutcome::Cancelled
            }
        };
        self.finalize_registered_guard(guard, outcome).await
    }

    async fn begin_registered_guard(
        &mut self,
        registered: Option<&RegisteredGenerator>,
        worktree: &Path,
        launch: &AgentLaunch,
    ) -> io::Result<Option<GeneratorGuard>> {
        let Some(generator) = registered else {
            return Ok(None);
        };
        let endpoint = launch.env_value("ROSEN_ENDPOINT").ok_or_else(|| {
            io::Error::other(format!(
                "Registered generator `{}` requires a Rosen workspace endpoint.",
                generator.command
            ))
        })?;
        begin_guard(endpoint, worktree, generator)
            .await
            .map(Some)
            .map_err(|reason| {
                let _ = writeln!(&self.output, "{reason}");
                io::Error::other(reason)
            })
    }

    async fn finalize_registered_guard(
        &mut self,
        guard: Option<GeneratorGuard>,
        outcome: ExecutionOutcome,
    ) -> ExecutionOutcome {
        let Some(guard) = guard else {
            return outcome;
        };
        let succeeded = matches!(&outcome, ExecutionOutcome::Exited(status) if status.success());
        match finish_guard(guard, succeeded).await {
            Ok(()) => outcome,
            Err(refusal) => {
                let _ = writeln!(&self.output, "\n{refusal}");
                ExecutionOutcome::Failed(io::Error::other(refusal))
            }
        }
    }

    async fn acquire_turn(
        &self,
        policy: AgentRun,
        worktree: &Path,
        directory: &Path,
        launch: &AgentLaunch,
        stop: &CancellationToken,
        sink: &RunStatusSink,
    ) -> Result<QueuedTurn, QueuedTurnError> {
        let class = self.command.run_class();
        let worktree = worktree.to_owned();
        let directory = directory.to_owned();
        let command_line = self.record.command_line();
        let queue_override = launch.env_value(QUEUE_ENV).map(PathBuf::from);
        let cancelled = stop.clone();
        let progress_sink = sink.clone();
        let ticket_sink = sink.clone();
        let result = tokio::task::spawn_blocking(move || {
            let request = QueuedTurnRequest {
                policy,
                class,
                worktree: &worktree,
                directory: &directory,
                command_line: &command_line,
                queue_override: queue_override.as_deref(),
                timeout: DEFAULT_WAIT_TIMEOUT,
            };
            acquire_with_ticket(
                &request,
                move || cancelled.is_cancelled(),
                move |reason| progress_sink.set_queue_reason(reason),
                move |ticket| ticket_sink.set_ticket(ticket),
            )
        })
        .await;
        sink.clear_ticket();
        result.map_err(|error| QueuedTurnError::Io(io::Error::other(error)))?
    }

    fn build_command(
        &self,
        rosen: &Path,
        directory: &Path,
        launch: &AgentLaunch,
        turn: &QueuedTurn,
        registered: Option<&RegisteredGenerator>,
    ) -> io::Result<tokio::process::Command> {
        let mut command = match turn {
            QueuedTurn::Rosen(rosen_turn) => {
                self.build_rosen_command(rosen, launch, rosen_turn.budget().jobs)?
            }
            QueuedTurn::Light { target, .. } => {
                self.build_light_command(rosen, launch, target, registered)?
            }
            QueuedTurn::Utility { .. } => {
                let clean_path = clean_toolchain_path(launch)?;
                let program = find_program_on_path(OsStr::new(&self.command.program), &clean_path);
                let mut command = hidden_command(program);
                launch.apply_environment(&mut command);
                apply_safe_command_env(&mut command, &self.command.env);
                command
                    .args(&self.command.arguments)
                    .env("PATH", clean_path)
                    .env_remove(BLOCK_VCS_ENV);
                command
            }
        };
        command
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(self.output.try_clone()?)
            .stderr(self.output.try_clone()?)
            .kill_on_drop(true);
        Ok(command)
    }

    fn build_light_command(
        &self,
        rosen: &Path,
        launch: &AgentLaunch,
        target: &Path,
        registered: Option<&RegisteredGenerator>,
    ) -> io::Result<tokio::process::Command> {
        let mut command = hidden_command(rosen);
        launch.apply_environment(&mut command);
        match registered.map(|item| &item.execution) {
            Some(GeneratorExecution::Direct {
                env,
                program,
                arguments,
            }) => {
                command
                    .args(["run", "--class", "build", "--", program])
                    .args(arguments);
                for (key, value) in env {
                    command.env(key, value);
                }
            }
            Some(GeneratorExecution::Shell(script)) => {
                if cfg!(windows) {
                    command.args(["run", "--class", "build", "--", "cmd", "/D", "/C", script]);
                } else {
                    command.args(["run", "--class", "build", "--", "sh", "-c", script]);
                }
            }
            None => {
                apply_safe_command_env(&mut command, &self.command.env);
                command
                    .args(["run", "--class", "build", "--", &self.command.program])
                    .args(&self.command.arguments);
            }
        }
        command
            .env("PATH", clean_toolchain_path(launch)?)
            .env_remove(BLOCK_VCS_ENV)
            .env(HELD_ENV, "build")
            .env("CARGO_BUILD_JOBS", "2")
            .env("CARGO_INCREMENTAL", "1")
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env("CARGO_TARGET_DIR", target);
        Ok(command)
    }

    fn build_rosen_command(
        &self,
        rosen: &Path,
        launch: &AgentLaunch,
        jobs: usize,
    ) -> io::Result<tokio::process::Command> {
        let mut command = hidden_command(rosen);
        launch.apply_environment(&mut command);
        apply_safe_command_env(&mut command, &self.command.env);
        // Drop Velocity's agent shims and VCS block once the top-level command passed policy
        // so test suites and tools (e.g. Go tests invoking `git` in temp dirs or
        // `git check-ref-format`) reach the host tools (#563).
        command
            .args([
                "run",
                "--class",
                self.command.class(),
                "--",
                &self.command.program,
            ])
            .args(&self.command.arguments)
            .env("PATH", clean_toolchain_path(launch)?)
            .env_remove(BLOCK_VCS_ENV)
            .env(HELD_ENV, self.command.class());
        let jobs = jobs.to_string();
        for variable in THREAD_VARIABLES {
            command.env(variable, &jobs);
        }
        Ok(command)
    }

    fn spawn_command(&self, command: tokio::process::Command) -> io::Result<super::AgentProcess> {
        // Keep clean_toolchain_path intact so queued runs never re-prepend Velocity's agent
        // shims onto PATH (#548, #563).
        spawn_owned_toolchain_command(command)
    }

    /// Unpacks Mathlib's prebuilt cache on demand before running `lake` so a fresh workspace
    /// never compiles Mathlib from source (#546).
    async fn prepare_lake_cache(
        &mut self,
        rosen: &Path,
        worktree: &Path,
        directory: &Path,
        launch: &AgentLaunch,
        turn: &QueuedTurn,
        stop: &CancellationToken,
    ) -> Option<ExecutionOutcome> {
        if !velocity_rosen::lean_cache::is_lake(&self.command.program) {
            return None;
        }
        let pending = velocity_rosen::lean_cache::pending_mathlib_cache(
            directory,
            Some(worktree),
            &self.command.arguments,
        )?;
        let _ = writeln!(
            &self.output,
            "==> Preparing Lean / Mathlib cache (lake exe cache get) in {}\n",
            pending.project.display()
        );
        let command = match self.build_lake_cache_command(rosen, &pending.project, launch, turn) {
            Ok(command) => command,
            Err(error) => return Some(ExecutionOutcome::Failed(error)),
        };
        let mut child = match spawn_owned_toolchain_command(command) {
            Ok(child) => child,
            Err(error) => return Some(ExecutionOutcome::Failed(error)),
        };
        let outcome = tokio::select! {
            status = child.wait() => status.map_or_else(ExecutionOutcome::Failed, ExecutionOutcome::Exited),
            () = stop.cancelled() => {
                let _ = stop_owned_process(&mut child).await;
                ExecutionOutcome::Cancelled
            }
        };
        match outcome {
            ExecutionOutcome::Exited(status) if status.success() => {
                velocity_rosen::lean_cache::record_mathlib_cache_ready(
                    &pending.project,
                    &pending.rev,
                );
                None
            }
            other => Some(other),
        }
    }

    fn build_lake_cache_command(
        &self,
        rosen: &Path,
        project: &Path,
        launch: &AgentLaunch,
        turn: &QueuedTurn,
    ) -> io::Result<tokio::process::Command> {
        let mut command = hidden_command(rosen);
        launch.apply_environment(&mut command);
        command
            .args([
                "run",
                "--class",
                self.command.class(),
                "--",
                &self.command.program,
                "exe",
                "cache",
                "get",
            ])
            .env("PATH", clean_toolchain_path(launch)?)
            .env_remove(BLOCK_VCS_ENV)
            .env(HELD_ENV, self.command.class());
        if let QueuedTurn::Rosen(rosen_turn) = turn {
            let jobs = rosen_turn.budget().jobs.to_string();
            for variable in THREAD_VARIABLES {
                command.env(variable, &jobs);
            }
        }
        command
            .current_dir(project)
            .stdin(Stdio::null())
            .stdout(self.output.try_clone()?)
            .stderr(self.output.try_clone()?)
            .kill_on_drop(true);
        Ok(command)
    }
}

pub(super) fn workspace_context(
    worktree: &Path,
    launch: &AgentLaunch,
) -> (Option<String>, Option<String>) {
    match workspace_head(worktree) {
        Some((change, head)) => (Some(change), Some(head)),
        None => (
            launch
                .env_value("ROSEN_CHANGE")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            None,
        ),
    }
}

pub(super) fn resolve_directory(worktree: &Path, directory: Option<&Path>) -> PathBuf {
    directory.map_or_else(|| worktree.to_owned(), |path| worktree.join(path))
}

struct FormattedOutcome {
    phase: QueuedRunPhase,
    succeeded: bool,
    include_log: bool,
    exit_code: Option<i32>,
    exit_status: String,
    message: String,
}

fn format_outcome(
    record: &QueuedRunRecord,
    title: &str,
    log: &Path,
    outcome: ExecutionOutcome,
) -> FormattedOutcome {
    let meta = outcome_meta(record);
    let log_path = log.display();
    match outcome {
        ExecutionOutcome::Exited(status) if status.success() => FormattedOutcome {
            phase: QueuedRunPhase::Completed,
            succeeded: true,
            include_log: false,
            exit_code: status.code(),
            exit_status: status.to_string(),
            message: format!("[Velocity] Passed · {title}{meta}\nFull log: {log_path}"),
        },
        ExecutionOutcome::Exited(status) => FormattedOutcome {
            phase: QueuedRunPhase::Failed,
            succeeded: false,
            include_log: true,
            exit_code: status.code(),
            exit_status: status.to_string(),
            message: format!("[Velocity] Failed ({status}) · {title}{meta}\nFull log: {log_path}"),
        },
        ExecutionOutcome::TimedOut(report) => FormattedOutcome {
            phase: QueuedRunPhase::TimedOut,
            succeeded: false,
            include_log: false,
            exit_code: None,
            exit_status: "timed out".to_owned(),
            message: format!(
                "[Velocity] Timed out waiting for queue turn · {title}{meta}\nFull log: {log_path}\n\n{report}"
            ),
        },
        ExecutionOutcome::Cancelled => FormattedOutcome {
            phase: QueuedRunPhase::Cancelled,
            succeeded: false,
            include_log: false,
            exit_code: None,
            exit_status: "cancelled".to_owned(),
            message: format!("[Velocity] Cancelled · {title}{meta}\nFull log: {log_path}"),
        },
        ExecutionOutcome::Failed(error) => FormattedOutcome {
            phase: QueuedRunPhase::Failed,
            succeeded: false,
            include_log: true,
            exit_code: None,
            exit_status: error.to_string(),
            message: format!("[Velocity] Failed ({error}) · {title}{meta}\nFull log: {log_path}"),
        },
    }
}

fn outcome_meta(record: &QueuedRunRecord) -> String {
    match (&record.change, &record.head) {
        (Some(change), Some(head)) => {
            format!("\nRun ID: {}\nChange: {change} (head {head})", record.id)
        }
        (Some(change), None) => format!("\nRun ID: {}\nChange: {change}", record.id),
        _ => format!("\nRun ID: {}", record.id),
    }
}

fn apply_safe_command_env(
    command: &mut tokio::process::Command,
    env: &std::collections::BTreeMap<String, String>,
) {
    for (key, value) in env {
        let upper = key.to_ascii_uppercase();
        if upper == "PATH"
            || upper.starts_with("ROSEN_")
            || upper.starts_with("VELOCITY_")
            || velocity_core::agent_env::is_denied_name(key)
        {
            continue;
        }
        command.env(key, value);
    }
}

fn clean_toolchain_path(launch: &AgentLaunch) -> io::Result<OsString> {
    let existing = launch
        .env_value("PATH")
        .map(OsString::from)
        .or_else(search_path)
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    toolchain_path_from(&existing)
}

pub(super) fn completion_notice(content: ContentBlock) -> SessionUpdate {
    SessionUpdate::UserMessageChunk(
        ContentChunk::new(content).meta(Meta::from_iter([(RUN_NOTICE.into(), true.into())])),
    )
}

/// This prompt chunk reports a queued command on behalf of Velocity.
pub fn is_queued_run_notice(update: &SessionUpdate) -> bool {
    matches!(update, SessionUpdate::UserMessageChunk(chunk)
        if chunk.meta.as_ref().and_then(|meta| meta.get(RUN_NOTICE)).and_then(serde_json::Value::as_bool) == Some(true))
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

pub(super) struct RunResult {
    id: ToolCallId,
    record: Option<QueuedRunRecord>,
    succeeded: bool,
    result: String,
    deliver_prompt: bool,
}

impl RunResult {
    /// A result that did not come from a queued command, such as a workflow run's.
    pub fn new(id: ToolCallId, succeeded: bool, result: String) -> Self {
        Self {
            id,
            record: None,
            succeeded,
            result,
            deliver_prompt: true,
        }
    }

    /// Attaches a timing/history record (used by `preflight` so check wait/run times persist, #619).
    pub fn with_record(
        id: ToolCallId,
        record: QueuedRunRecord,
        succeeded: bool,
        result: String,
    ) -> Self {
        Self {
            id,
            record: Some(record),
            succeeded,
            result,
            deliver_prompt: true,
        }
    }

    pub fn record(&self) -> Option<&QueuedRunRecord> {
        self.record.as_ref()
    }

    pub fn deliver_prompt(&self) -> bool {
        self.deliver_prompt
    }

    pub fn into_update(self) -> (SessionUpdate, ContentBlock, bool) {
        let status = if self.succeeded {
            ToolCallStatus::Completed
        } else {
            ToolCallStatus::Failed
        };
        let message = ContentBlock::from(self.result);
        let fields = ToolCallUpdateFields::new()
            .status(status)
            .content(vec![ToolCallContent::from(message.clone())]);
        (
            SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(self.id, fields)),
            message,
            self.deliver_prompt,
        )
    }
}

/// Reads a bounded diagnostic + tail excerpt from a run's log file for `run_status`.
pub(super) fn read_log_excerpt(log: &Path) -> Option<String> {
    let output = failure_output(log).ok()?;
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(velocity_core::text_budget::prompt_excerpt(
        trimmed,
        STATUS_EXCERPT_BYTES,
        "\n[Log excerpt. Read the full log at the path above.]",
    ))
}

/// Cargo can print a long follow-up build after the first compiler error. Preserve the
/// diagnostics even if the last lines of the run no longer contain them.
fn failure_output(log: &Path) -> io::Result<String> {
    let mut reader = BufReader::new(File::open(log)?);
    let mut bytes = Vec::new();
    let mut diagnostics = Vec::new();
    let mut tail = VecDeque::new();
    while reader.by_ref().take(2048).read_until(b'\n', &mut bytes)? != 0 {
        if !bytes.ends_with(b"\n") {
            reader.skip_until(b'\n')?;
        }
        let line = String::from_utf8_lossy(&bytes);
        let line: String = line.trim_end().chars().take(500).collect();
        if diagnostics.len() < 24 && is_diagnostic(&line) {
            diagnostics.push(line.clone());
        }
        if tail.len() == 16 {
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
        || line.starts_with("FAILED")
        || line.starts_with("---- ")
        || line.starts_with("-->")
        || line.contains(" ... FAILED")
        || line.contains(": error:")
        || line.contains("panicked at")
}

#[cfg(test)]
mod tests {
    use super::*;
    use velocity_core::queued_runs::QueuedRunClass;

    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new() -> io::Result<Self> {
            let path = std::env::temp_dir()
                .join(format!("velocity-queued-run-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command_path_env(command: &tokio::process::Command) -> Option<OsString> {
        command
            .as_std()
            .get_envs()
            .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
            .and_then(|(_, value)| value.map(OsStr::to_owned))
    }

    fn resolve_program_on(program: &str, path: &OsStr) -> Option<PathBuf> {
        let exe = format!("{program}{}", std::env::consts::EXE_SUFFIX);
        std::env::split_paths(path)
            .map(|folder| folder.join(&exe))
            .find(|candidate| candidate.is_file())
    }

    fn make_light_turn(folder: &Path) -> io::Result<QueuedTurn> {
        let project = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(folder.join("project.lock"))?;
        let slot = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(folder.join("slot.lock"))?;
        Ok(QueuedTurn::Light {
            _project: project,
            _slot: slot,
            target: folder.join("target"),
        })
    }

    struct TestEnv {
        shim: PathBuf,
        toolchain: PathBuf,
        worktree: PathBuf,
        cargo: String,
        git: String,
        launch: AgentLaunch,
    }

    fn setup_shim_and_toolchain(dir: &Path) -> io::Result<TestEnv> {
        let shim = dir.join("shim");
        let toolchain = dir.join("toolchain");
        let worktree = dir.join("worktree");
        std::fs::create_dir_all(&shim)?;
        std::fs::create_dir_all(&toolchain)?;
        std::fs::create_dir_all(&worktree)?;
        let cargo = format!("cargo{}", std::env::consts::EXE_SUFFIX);
        let git = format!("git{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(shim.join("velocity-program"), b"velocity")?;
        std::fs::write(shim.join(&cargo), b"shim")?;
        std::fs::write(shim.join(&git), b"shim")?;
        std::fs::write(toolchain.join(&cargo), b"real")?;
        std::fs::write(toolchain.join(&git), b"real")?;
        let joined = std::env::join_paths([&shim, &toolchain]).map_err(io::Error::other)?;
        let mut launch = AgentLaunch::new(PathBuf::from("agent"));
        launch
            .env
            .push(("Path".into(), joined.to_string_lossy().into_owned()));
        launch.env.push((BLOCK_VCS_ENV.into(), "1".into()));
        Ok(TestEnv {
            shim,
            toolchain,
            worktree,
            cargo,
            git,
            launch,
        })
    }

    fn cargo_check_command() -> RunCommand {
        let record = QueuedRunRecord::new(
            "run-test".into(),
            "cargo".into(),
            vec!["check".into(), "-p".into(), "velocity-app".into()],
            None,
            QueuedRunClass::Build,
        );
        RunCommand::from_record(&record)
    }

    fn go_test_command() -> RunCommand {
        let record = QueuedRunRecord::new(
            "run-go-test".into(),
            "go".into(),
            vec!["-C".into(), "server".into(), "test".into(), ".".into()],
            None,
            QueuedRunClass::Build,
        );
        RunCommand::from_record(&record)
    }

    // Risk (#548): a Light queued run (fresh or restored after a restart, including Windows
    // mixed-case `Path`) must drop the managed shim folder so its child resolves `cargo` to
    // the real toolchain instead of re-entering the shim and deadlocking on the light lock.
    #[test]
    fn light_queued_run_child_resolves_cargo_to_real_toolchain() -> io::Result<()> {
        let dir = TempFolder::new()?;
        let env = setup_shim_and_toolchain(dir.path())?;
        let fresh = QueuedRun::new(cargo_check_command(), &env.worktree, &env.launch)?;
        let restored = QueuedRun::restored(&fresh.record(), &env.worktree, &env.launch)?;
        let turn = make_light_turn(dir.path())?;
        let rosen = dir.path().join("rosen");

        for run in [&fresh, &restored] {
            let built = run.build_command(&rosen, &env.worktree, &env.launch, &turn, None)?;
            let child_path = command_path_env(&built)
                .ok_or_else(|| io::Error::other("Light command missing PATH"))?;
            let entries: Vec<PathBuf> = std::env::split_paths(&child_path).collect();
            assert!(
                !entries.contains(&env.shim),
                "shim survived on PATH: {entries:?}"
            );
            assert_eq!(
                resolve_program_on("cargo", &child_path),
                Some(env.toolchain.join(&env.cargo)),
                "cargo did not resolve to real toolchain"
            );
            let _ = std::fs::remove_file(&run.log);
        }
        Ok(())
    }

    // Risk (#563): ordinary queued runs (such as `go -C server test .`) must drop Velocity's
    // agent shim folder and VELOCITY_BLOCK_GIT_GH so child tests invoking `git` reach the
    // host git binary instead of failing with the Rosen git shim refusal.
    #[test]
    fn ordinary_rosen_queued_run_resolves_git_to_real_toolchain() -> io::Result<()> {
        let dir = TempFolder::new()?;
        let env = setup_shim_and_toolchain(dir.path())?;
        let fresh = QueuedRun::new(go_test_command(), &env.worktree, &env.launch)?;
        let restored = QueuedRun::restored(&fresh.record(), &env.worktree, &env.launch)?;
        let rosen = dir.path().join("rosen");

        for run in [&fresh, &restored] {
            let built = run.build_rosen_command(&rosen, &env.launch, 2)?;
            let child_path = command_path_env(&built)
                .ok_or_else(|| io::Error::other("Rosen command missing PATH"))?;
            let entries: Vec<PathBuf> = std::env::split_paths(&child_path).collect();
            assert!(
                !entries.contains(&env.shim),
                "shim survived on PATH: {entries:?}"
            );
            assert_eq!(
                resolve_program_on("git", &child_path),
                Some(env.toolchain.join(&env.git)),
                "git did not resolve to real host binary"
            );
            assert!(
                built
                    .as_std()
                    .get_envs()
                    .any(|(name, value)| name == BLOCK_VCS_ENV && value.is_none()),
                "VELOCITY_BLOCK_GIT_GH was not removed"
            );
            let _ = std::fs::remove_file(&run.log);
        }
        Ok(())
    }

    // Risk (#548, #563): spawning any queued run must preserve `clean_toolchain_path` and
    // never run `extend_search_path` (which would re-prepend Velocity's agent shim folder).
    #[tokio::test]
    async fn spawn_command_keeps_shim_off_queued_run_path() -> io::Result<()> {
        let dir = TempFolder::new()?;
        let env = setup_shim_and_toolchain(dir.path())?;
        let run = QueuedRun::new(go_test_command(), &env.worktree, &env.launch)?;
        let clean_path = clean_toolchain_path(&env.launch)?;
        let mut probe = if cfg!(windows) {
            let shell = std::env::var_os("COMSPEC")
                .unwrap_or_else(|| OsString::from(r"C:\Windows\System32\cmd.exe"));
            let mut command = hidden_command(shell);
            command.args(["/C", "echo %PATH%"]);
            command
        } else {
            let mut command = hidden_command("/bin/sh");
            command.args(["-c", "printf '%s' \"$PATH\""]);
            command
        };
        probe
            .env("PATH", &clean_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let mut child = run.spawn_command(probe)?;
        let mut stdout = child
            .stdout()
            .take()
            .ok_or_else(|| io::Error::other("missing probe stdout"))?;
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes).await?;
        let status = child.wait().await?;
        let _ = std::fs::remove_file(&run.log);
        assert!(status.success(), "probe exited with {status}");
        let observed = OsString::from(String::from_utf8_lossy(&bytes).trim());
        let entries: Vec<PathBuf> = std::env::split_paths(&observed).collect();
        assert!(
            !entries.contains(&env.shim),
            "shim survived on child PATH: {entries:?}"
        );
        assert_eq!(
            resolve_program_on("cargo", &observed),
            Some(env.toolchain.join(&env.cargo)),
            "spawned child PATH did not resolve cargo to real toolchain: {observed:?}"
        );
        assert_eq!(
            resolve_program_on("git", &observed),
            Some(env.toolchain.join(&env.git)),
            "spawned child PATH did not resolve git to real toolchain: {observed:?}"
        );
        Ok(())
    }

    // Risk (#582): a RegisteredGenerator queued run must pass its registered environment
    // variables (e.g. `UPDATE_FIXTURES=1`) and exact program/arguments in the light run queue
    // with Velocity's agent shim folder removed from PATH.
    #[test]
    fn registered_generator_light_command_applies_env_and_clean_path() -> io::Result<()> {
        let dir = TempFolder::new()?;
        let env = setup_shim_and_toolchain(dir.path())?;
        let record = QueuedRunRecord::new(
            "run-gen".into(),
            "UPDATE_FIXTURES=1".into(),
            vec![
                "cargo".into(),
                "test".into(),
                "-p".into(),
                "rosen-server".into(),
                "--lib".into(),
                "collapse_fixtures".into(),
            ],
            None,
            QueuedRunClass::Build,
        );
        let run = QueuedRun::new(RunCommand::from_record(&record), &env.worktree, &env.launch)?;
        let turn = make_light_turn(dir.path())?;
        let rosen = dir.path().join("rosen");
        let registered = RegisteredGenerator {
            command: "UPDATE_FIXTURES=1 cargo test -p rosen-server --lib collapse_fixtures".into(),
            execution: GeneratorExecution::Direct {
                env: vec![("UPDATE_FIXTURES".into(), "1".into())],
                program: "cargo".into(),
                arguments: vec![
                    "test".into(),
                    "-p".into(),
                    "rosen-server".into(),
                    "--lib".into(),
                    "collapse_fixtures".into(),
                ],
            },
            patterns: Vec::new(),
        };
        let built =
            run.build_command(&rosen, &env.worktree, &env.launch, &turn, Some(&registered))?;
        let _ = std::fs::remove_file(&run.log);
        let args: Vec<&OsStr> = built.as_std().get_args().collect();
        assert_eq!(
            args,
            vec![
                OsStr::new("run"),
                OsStr::new("--class"),
                OsStr::new("build"),
                OsStr::new("--"),
                OsStr::new("cargo"),
                OsStr::new("test"),
                OsStr::new("-p"),
                OsStr::new("rosen-server"),
                OsStr::new("--lib"),
                OsStr::new("collapse_fixtures"),
            ]
        );
        assert!(
            built
                .as_std()
                .get_envs()
                .any(|(name, value)| name == "UPDATE_FIXTURES" && value == Some(OsStr::new("1"))),
            "UPDATE_FIXTURES=1 was not set on the light generator command"
        );
        let child_path = command_path_env(&built)
            .ok_or_else(|| io::Error::other("RegisteredGenerator command missing PATH"))?;
        let entries: Vec<PathBuf> = std::env::split_paths(&child_path).collect();
        assert!(
            !entries.contains(&env.shim),
            "shim survived on PATH: {entries:?}"
        );
        Ok(())
    }
}
