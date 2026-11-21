//! Run the command behind Rosen and keep the complete output outside the workspace.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, Meta, SessionUpdate, ToolCall, ToolCallContent, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use tokio_util::sync::CancellationToken;
use velocity_core::queued_runs::{QueuedRunPhase, QueuedRunRecord};
use velocity_rosen::agent_run::{AgentRun, toolchain_path_from};
use velocity_rosen::queued_turn::{
    DEFAULT_WAIT_TIMEOUT, HELD_ENV, QUEUE_ENV, QueueTimeoutReport, QueuedTurn, QueuedTurnError,
    QueuedTurnRequest, THREAD_VARIABLES, acquire_with_progress, workspace_head,
};

use super::AgentLaunch;
use super::process::{hidden_command, spawn_owned_command, stop_owned_process};
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
    output: File,
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

    pub async fn finish(
        mut self,
        rosen: &Path,
        policy: AgentRun,
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
        sink: RunStatusSink,
    ) -> RunResult {
        let outcome = self.run(rosen, policy, worktree, launch, stop, &sink).await;
        let cancelled_by_user = sink.is_cancelled_by_user();
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

    async fn run(
        &mut self,
        rosen: &Path,
        policy: AgentRun,
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
            Err(QueuedTurnError::Cancelled) => return ExecutionOutcome::Cancelled,
            Err(QueuedTurnError::TimedOut(report)) => {
                let _ = writeln!(&self.output, "{report}");
                return ExecutionOutcome::TimedOut(report);
            }
            Err(QueuedTurnError::Io(error)) => return ExecutionOutcome::Failed(error),
        };
        let (change, head) = workspace_context(worktree, launch);
        if change.is_some() {
            self.record.change = change;
        }
        if head.is_some() {
            self.record.head = head;
        }
        sink.set_running(self.record.change.clone(), self.record.head.clone());
        let command = match self.build_command(rosen, &directory, launch, &turn) {
            Ok(command) => command,
            Err(error) => return ExecutionOutcome::Failed(error),
        };
        let mut child = match spawn_owned_command(command) {
            Ok(child) => child,
            Err(error) => return ExecutionOutcome::Failed(error),
        };
        let outcome = tokio::select! {
            status = child.wait() => status.map_or_else(ExecutionOutcome::Failed, ExecutionOutcome::Exited),
            () = stop.cancelled() => {
                let _ = stop_owned_process(&mut child).await;
                ExecutionOutcome::Cancelled
            }
        };
        drop(turn);
        outcome
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
        tokio::task::spawn_blocking(move || {
            let request = QueuedTurnRequest {
                policy,
                class,
                worktree: &worktree,
                directory: &directory,
                command_line: &command_line,
                queue_override: queue_override.as_deref(),
                timeout: DEFAULT_WAIT_TIMEOUT,
            };
            acquire_with_progress(
                &request,
                move || cancelled.is_cancelled(),
                move |reason| progress_sink.set_queue_reason(reason),
            )
        })
        .await
        .map_err(|error| QueuedTurnError::Io(io::Error::other(error)))?
    }

    fn build_command(
        &self,
        rosen: &Path,
        directory: &Path,
        launch: &AgentLaunch,
        turn: &QueuedTurn,
    ) -> io::Result<tokio::process::Command> {
        let mut command = match turn {
            QueuedTurn::Rosen(rosen_turn) => {
                let mut command = hidden_command(rosen);
                launch.apply_environment(&mut command);
                command
                    .args([
                        "run",
                        "--class",
                        self.command.class(),
                        "--",
                        &self.command.program,
                    ])
                    .args(&self.command.arguments)
                    .env(HELD_ENV, self.command.class());
                let jobs = rosen_turn.budget().jobs.to_string();
                for variable in THREAD_VARIABLES {
                    command.env(variable, &jobs);
                }
                command
            }
            QueuedTurn::Light { target, .. } => {
                let mut command = hidden_command(rosen);
                launch.apply_environment(&mut command);
                command
                    .args(["run", "--class", "build", "--", &self.command.program])
                    .args(&self.command.arguments)
                    .env("PATH", clean_toolchain_path(launch)?)
                    .env(HELD_ENV, "build")
                    .env("CARGO_BUILD_JOBS", "2")
                    .env("CARGO_INCREMENTAL", "1")
                    .env("RUSTC_WRAPPER", "")
                    .env("RUSTC_WORKSPACE_WRAPPER", "")
                    .env("CARGO_TARGET_DIR", target);
                command
            }
            QueuedTurn::Utility { .. } => {
                let mut command = hidden_command(&self.command.program);
                launch.apply_environment(&mut command);
                command
                    .args(&self.command.arguments)
                    .env("PATH", clean_toolchain_path(launch)?);
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

fn clean_toolchain_path(launch: &AgentLaunch) -> io::Result<OsString> {
    let existing = launch
        .env_value("PATH")
        .map(OsString::from)
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
