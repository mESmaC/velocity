//! Run the command behind Rosen and keep the complete output outside the workspace.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, Meta, SessionUpdate, ToolCall, ToolCallContent, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};
use tokio_util::sync::CancellationToken;

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

impl QueuedRun {
    pub fn new(command: RunCommand) -> io::Result<Self> {
        let id = format!("{RUN_ID_PREFIX}{}", uuid::Uuid::new_v4());
        let log = std::env::temp_dir().join(format!("{id}.log"));
        let output = create_log(&log)?;
        let title = format!(
            "rosen run -- {} {}",
            command.program,
            command.arguments.join(" ")
        )
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
        let mut characters = title.chars();
        let mut title: String = characters.by_ref().take(240).collect();
        if characters.next().is_some() {
            title.push('…');
        }
        Ok(Self {
            id: ToolCallId::new(id),
            title,
            log,
            command,
            output,
        })
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
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
    ) -> RunResult {
        let status = self.run(rosen, worktree, launch, stop).await;
        let succeeded = status.as_ref().is_ok_and(|status| status.success());
        let outcome = match status {
            Ok(_) if succeeded => "Passed".to_owned(),
            Ok(status) => format!("Failed ({status})"),
            Err(error) => format!("Failed ({error})"),
        };
        let mut result = format!(
            "[Velocity] {outcome} · {}\nFull log: {}",
            self.title,
            self.log.display()
        );
        if !succeeded {
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
        RunResult {
            id: self.id,
            succeeded,
            result,
        }
    }

    async fn run(
        &self,
        rosen: &Path,
        worktree: &Path,
        launch: &AgentLaunch,
        stop: CancellationToken,
    ) -> io::Result<ExitStatus> {
        if stop.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Command cancelled",
            ));
        }
        let mut command = hidden_command(rosen);
        launch.apply_environment(&mut command);
        let directory = self
            .command
            .directory
            .as_ref()
            .map_or_else(|| worktree.to_owned(), |path| worktree.join(path));
        writeln!(
            &self.output,
            "{}\nDirectory: {}\n",
            self.title,
            directory.display()
        )?;
        command
            .args([
                "run",
                "--class",
                self.command.class(),
                "--",
                &self.command.program,
            ])
            .args(&self.command.arguments)
            .current_dir(directory)
            .env_remove("ROSEN_RUN_HELD")
            .stdin(Stdio::null())
            .stdout(self.output.try_clone()?)
            .stderr(self.output.try_clone()?)
            .kill_on_drop(true);
        let mut child = spawn_owned_command(command)?;
        tokio::select! {
            status = child.wait() => status,
            () = stop.cancelled() => {
                stop_owned_process(&mut child).await?;
                Err(io::Error::new(io::ErrorKind::Interrupted, "Command cancelled"))
            }
        }
    }
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
    succeeded: bool,
    result: String,
}

impl RunResult {
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
