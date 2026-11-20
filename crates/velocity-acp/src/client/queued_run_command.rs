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
use velocity_core::queued_runs::QueuedRunRecord;
use velocity_rosen::agent_run::{AgentRun, toolchain_path_from};
use velocity_rosen::queued_turn::{
    DEFAULT_WAIT_TIMEOUT, HELD_ENV, QUEUE_ENV, QueueTimeoutReport, QueuedTurn, QueuedTurnError,
    QueuedTurnRequest, THREAD_VARIABLES, acquire,
};

use super::AgentLaunch;
use super::process::{hidden_command, spawn_owned_command, stop_owned_process};
use super::queued_run_tool::RunCommand;

const RUN_ID_PREFIX: &str = "velocity-run-";
const RUN_NOTICE: &str = "velocity.queued_run_result";

pub(super) struct QueuedRun {
    id: ToolCallId,
    title: String,
    pub log: PathBuf,
    command: RunCommand,
    output: File,
}

enum ExecutionOutcome {
    Exited(ExitStatus),
    TimedOut(Box<QueueTimeoutReport>),
    Failed(io::Error),
}

impl QueuedRun {
    pub fn new(command: RunCommand) -> io::Result<Self> {
        let id = format!("{RUN_ID_PREFIX}{}", uuid::Uuid::new_v4());
        let log = std::env::temp_dir().join(format!("{id}.log"));
        Self::with_id(id, log, command)
    }

    /// Reconstructs a persisted run after a Velocity restart, preserving its identity
    /// while giving the restarted attempt a fresh log file.
    pub fn restored(record: &QueuedRunRecord) -> io::Result<Self> {
        let log = std::env::temp_dir().join(format!(
            "{}-restored-{}.log",
            record.id,
            uuid::Uuid::new_v4()
        ));
        Self::with_id(record.id.clone(), log, RunCommand::from_record(record))
    }

    fn with_id(id: String, log: PathBuf, command: RunCommand) -> io::Result<Self> {
        let output = create_log(&log)?;
        let record = command.to_record(id.clone());
        let title = record.title();
        Ok(Self {
            id: ToolCallId::new(id),
            title,
            log,
            command,
            output,
        })
    }

    pub fn record(&self) -> QueuedRunRecord {
        self.command.to_record(self.id.0.to_string())
    }

    pub fn started(&self) -> SessionUpdate {
        SessionUpdate::ToolCall(
            ToolCall::new(self.id.clone(), self.title.clone())
                .kind(ToolKind::Execute)
                .status(ToolCallStatus::InProgress),
        )
    }

    pub async fn finish(
        self,
        rosen: &Path,
        policy: AgentRun,
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
    ) -> RunResult {
        let outcome = self.run(rosen, policy, worktree, launch, stop).await;
        let persisted_id = Some(self.id.0.to_string());
        let (succeeded, include_log, mut result) = format_outcome(&self.title, &self.log, outcome);
        if include_log {
            let log = self.log;
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
        RunResult {
            id: self.id,
            persisted_id,
            succeeded,
            result,
        }
    }

    async fn run(
        &self,
        rosen: &Path,
        policy: AgentRun,
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
    ) -> ExecutionOutcome {
        if stop.is_cancelled() {
            return ExecutionOutcome::Failed(cancelled_error());
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
            "{}\nDirectory: {}\n",
            self.title,
            directory.display()
        ) {
            return ExecutionOutcome::Failed(error);
        }
        let turn = match self
            .acquire_turn(policy, worktree, &directory, launch, &stop)
            .await
        {
            Ok(turn) => turn,
            Err(QueuedTurnError::Cancelled) => return ExecutionOutcome::Failed(cancelled_error()),
            Err(QueuedTurnError::TimedOut(report)) => {
                let _ = writeln!(&self.output, "{report}");
                return ExecutionOutcome::TimedOut(report);
            }
            Err(QueuedTurnError::Io(error)) => return ExecutionOutcome::Failed(error),
        };
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
                ExecutionOutcome::Failed(cancelled_error())
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
    ) -> Result<QueuedTurn, QueuedTurnError> {
        let class = self.command.run_class();
        let worktree = worktree.to_owned();
        let directory = directory.to_owned();
        let command_line = format!(
            "{} {}",
            self.command.program,
            self.command.arguments.join(" ")
        )
        .trim()
        .to_owned();
        let queue_override = launch.env_value(QUEUE_ENV).map(PathBuf::from);
        let cancelled = stop.clone();
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
            acquire(&request, move || cancelled.is_cancelled())
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

pub(super) fn resolve_directory(worktree: &Path, directory: Option<&Path>) -> PathBuf {
    directory.map_or_else(|| worktree.to_owned(), |path| worktree.join(path))
}

fn cancelled_error() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "Command cancelled")
}

fn format_outcome(title: &str, log: &Path, outcome: ExecutionOutcome) -> (bool, bool, String) {
    match outcome {
        ExecutionOutcome::Exited(status) if status.success() => (
            true,
            false,
            format!("[Velocity] Passed · {title}\nFull log: {}", log.display()),
        ),
        ExecutionOutcome::Exited(status) => (
            false,
            true,
            format!(
                "[Velocity] Failed ({status}) · {title}\nFull log: {}",
                log.display()
            ),
        ),
        ExecutionOutcome::TimedOut(report) => (
            false,
            false,
            format!(
                "[Velocity] Timed out waiting for queue turn · {title}\nFull log: {}\n\n{report}",
                log.display()
            ),
        ),
        ExecutionOutcome::Failed(error) => (
            false,
            true,
            format!(
                "[Velocity] Failed ({error}) · {title}\nFull log: {}",
                log.display()
            ),
        ),
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
    persisted_id: Option<String>,
    succeeded: bool,
    result: String,
}

impl RunResult {
    /// A result that did not come from a queued command, such as a workflow run's.
    pub fn new(id: ToolCallId, succeeded: bool, result: String) -> Self {
        Self {
            id,
            persisted_id: None,
            succeeded,
            result,
        }
    }

    pub fn persisted_id(&self) -> Option<&str> {
        self.persisted_id.as_deref()
    }

    pub fn into_update(self) -> (SessionUpdate, ContentBlock) {
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
        )
    }
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
