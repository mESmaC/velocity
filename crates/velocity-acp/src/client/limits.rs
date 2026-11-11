//! Reading subscription limits outside ACP (F15 Limit sources), from the same sources T3 Code
//! reads, through each CLI's own login: Claude Code's `get_usage` control request and Codex's
//! app-server `account/rateLimits/read`. Velocity never sees or stores a token here.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::ClientError;
use super::process::{self, AgentProcess};
use super::program_path::find_program;
use crate::usage_limits::{LimitReading, claude_usage_reading, codex_usage_reading};

/// Claude Code's usage report: the 5-hour and weekly windows. `Ok(None)` when it reports no
/// limits for this login.
pub async fn read_claude_usage(
    directory: &Path,
    deadline: Duration,
) -> Result<Option<LimitReading>, ClientError> {
    let requests = [
        json!({"type": "control_request", "request_id": "velocity-init", "request": {"subtype": "initialize"}}),
        json!({"type": "control_request", "request_id": "velocity-usage", "request": {"subtype": "get_usage"}}),
    ];
    let arguments = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
    ];
    let answer = exchange(
        "claude",
        &arguments,
        directory,
        &requests,
        deadline,
        |line| {
            let response = line.get("response")?;
            (line.get("type")? == "control_response"
                && response.get("request_id")? == "velocity-usage")
                .then(|| response.get("response").cloned().unwrap_or(Value::Null))
        },
    )
    .await?;
    Ok(claude_usage_reading(&answer))
}

/// Codex's rate-limit windows from its app server.
pub async fn read_codex_usage(
    directory: &Path,
    deadline: Duration,
) -> Result<Option<LimitReading>, ClientError> {
    let version = env!("CARGO_PKG_VERSION");
    let requests = [
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "velocity", "version": version}}}),
        json!({"method": "initialized"}),
        json!({"id": 2, "method": "account/rateLimits/read"}),
    ];
    let answer = exchange(
        "codex",
        &["app-server"],
        directory,
        &requests,
        deadline,
        |line| (line.get("id")? == 2).then(|| line.get("result").cloned().unwrap_or(Value::Null)),
    )
    .await?;
    Ok(codex_usage_reading(&answer))
}

/// Starts `program`, writes each request as one JSON line, and returns the first line
/// `answer` picks. The process tree is stopped afterwards, whatever happened.
async fn exchange(
    program: &str,
    arguments: &[&str],
    directory: &Path,
    requests: &[Value],
    deadline: Duration,
    answer: impl Fn(&Value) -> Option<Value>,
) -> Result<Value, ClientError> {
    let mut command = process::hidden_command(find_program(OsStr::new(program)));
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = process::spawn_owned_command(command).map_err(|source| ClientError::Spawn {
        program: program.to_owned(),
        source,
    })?;
    let outcome = tokio::time::timeout(deadline, converse(&mut child, requests, &answer)).await;
    tokio::spawn(async move {
        let _ = process::stop_owned_process(&mut child).await;
    });
    outcome.map_err(|_| ClientError::TimedOut(deadline))?
}

async fn converse(
    child: &mut AgentProcess,
    requests: &[Value],
    answer: &impl Fn(&Value) -> Option<Value>,
) -> Result<Value, ClientError> {
    let mut stdin = child
        .stdin()
        .take()
        .ok_or(ClientError::MissingPipe("stdin"))?;
    let stdout = child
        .stdout()
        .take()
        .ok_or(ClientError::MissingPipe("stdout"))?;
    for request in requests {
        stdin.write_all(format!("{request}\n").as_bytes()).await?;
    }
    stdin.flush().await?;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        // Other lines (hooks, notifications) and non-JSON output are skipped.
        if let Ok(value) = serde_json::from_str::<Value>(&line)
            && let Some(found) = answer(&value)
        {
            return Ok(found);
        }
    }
    Err(ClientError::Usage(
        "the process exited before answering".into(),
    ))
}
