//! The stdio MCP subprocess hands requests to its owning ACP session over loopback.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use velocity_core::queued_runs::{QueuedRunClass, QueuedRunRecord};

use super::preflight_tool::PreflightRequest;
use super::workflow_run_tool::WorkflowRequest;

pub const QUEUED_RUN_SWITCH: &str = "--queued-run-mcp";
pub const QUEUED_RUN_SERVER: &str = "velocity-run";
pub const QUEUED_RUN_TOOL: &str = "queue_run";
pub const RUN_STATUS_TOOL: &str = "run_status";
pub const RUN_CANCEL_TOOL: &str = "run_cancel";
pub const QUEUED_RUN_TOKEN_ENV: &str = "VELOCITY_QUEUED_RUN_TOKEN";
pub const QUEUED_RUN_PORT_ENV: &str = "VELOCITY_QUEUED_RUN_PORT";
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REQUEST_BYTES: u64 = 128 * 1024;

pub(super) const INSTRUCTIONS: &str = "Queue checks and builds through `velocity-run` \
    (including `cargo check -p <crate>`, Lean checks, `cargo metadata --format-version 1` \
    and `pnpm -C ui bindings`). Pass the program and its arguments separately. \
    `queue_run` returns a durable `run_id`, queue position and estimated wait; inspect with \
    `run_status` or cancel with `run_cancel`. Run `preflight` before naming a change. \
    After a command or preflight queues, end your turn; Velocity sends the result and log \
    path in your next prompt.";

#[derive(Deserialize, Serialize)]
pub(super) struct Request {
    pub token: String,
    #[serde(flatten)]
    pub work: Work,
}

/// What a tool asks the session's queue for: `command`, `workflow`, `status`, `cancel` or `preflight`.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Work {
    Command(RunCommand),
    Workflow(WorkflowRequest),
    Status(RunStatusRequest),
    Cancel(RunCancelRequest),
    Preflight(PreflightRequest),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(super) struct RunStatusRequest {
    #[serde(default)]
    pub run_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RunCancelRequest {
    pub run_id: String,
}

/// Durable metadata returned immediately when `queue_run` accepts a command.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct QueuedRunAccepted {
    pub run_id: String,
    pub log: PathBuf,
    pub phase: String,
    pub queue_position: u32,
    pub estimated_wait_seconds: u64,
    pub estimated_wait: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

/// What the queue answers an accepted request with.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Accepted {
    /// A queued command's durable ID, queue estimate, workspace head and log path.
    Queued(QueuedRunAccepted),
    /// The file a preflight's output goes to.
    Log(PathBuf),
    /// The workflow run as the thread shows it.
    Workflow(String),
    /// A structured JSON response (`run_status`, `run_cancel`).
    Json(Value),
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct RunCommand {
    pub program: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub directory: Option<PathBuf>,
    #[serde(default)]
    class: QueuedRunClass,
}

impl RunCommand {
    pub fn class(&self) -> &'static str {
        self.class.as_str()
    }

    pub fn run_class(&self) -> QueuedRunClass {
        self.class
    }

    pub fn from_record(record: &QueuedRunRecord) -> Self {
        Self {
            program: record.program.clone(),
            arguments: record.arguments.clone(),
            directory: record.directory.as_ref().map(PathBuf::from),
            class: record.class,
        }
    }

    pub fn to_record(&self, id: String) -> QueuedRunRecord {
        QueuedRunRecord::new(
            id,
            self.program.clone(),
            self.arguments.clone(),
            self.directory
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            self.class,
        )
    }
}

pub fn queued_run_tool() -> Value {
    json!({
        "name": QUEUED_RUN_TOOL,
        "description": INSTRUCTIONS,
        "inputSchema": {
            "type": "object",
            "properties": {
                "program": {"type": "string", "description": "Executable, e.g. cargo, lake or pwsh"},
                "arguments": {"type": "array", "items": {"type": "string"}},
                "directory": {"type": "string", "description": "Working directory, relative to the session folder by default"},
                "class": {"type": "string", "enum": ["build", "landing", "verify"], "default": "build"}
            },
            "required": ["program"]
        }
    })
}

pub fn run_status_tool() -> Value {
    json!({
        "name": RUN_STATUS_TOOL,
        "description": "Inspect a queued run by its durable `run_id`, or list all known runs for this thread when `run_id` is omitted. Reports the run's phase, queueing reason, queue position and estimated wait (while queued), the Rosen change and head checked, the exit status, a log excerpt and the log's full path. Works across restarts so you can recover a missed result.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "run_id": {"type": "string", "description": "Durable run ID returned by `queue_run` or listed in a restart notice. Omit to list known runs for this thread."}
            }
        }
    })
}

pub fn run_cancel_tool() -> Value {
    json!({
        "name": RUN_CANCEL_TOOL,
        "description": "Cancel a pending or running `queue_run` command by its durable `run_id`. Stops waiting in the queue or terminates the running command cleanly. Returns whether the run was cancelled or had already finished.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "run_id": {"type": "string", "description": "Durable run ID returned by `queue_run`"}
            },
            "required": ["run_id"]
        }
    })
}

pub fn request_queued_run(port: u16, token: &str, arguments: Value) -> Result<Value, String> {
    let command: RunCommand =
        serde_json::from_value(arguments).map_err(|error| error.to_string())?;
    if command.program.trim().is_empty() {
        return Err("Provide a program to run".into());
    }
    let request = Request {
        token: token.to_owned(),
        work: Work::Command(command),
    };
    submit::<QueuedRunAccepted>(port, &request, REQUEST_TIMEOUT)
        .map_err(|error| format!("Could not queue the command: {error}"))?
        .map(|accepted| {
            json!({
                "run_id": accepted.run_id,
                "queued": true,
                "phase": accepted.phase,
                "queue_position": accepted.queue_position,
                "estimated_wait_seconds": accepted.estimated_wait_seconds,
                "estimated_wait": accepted.estimated_wait,
                "queue_reason": accepted.queue_reason,
                "change": accepted.change,
                "head": accepted.head,
                "log": accepted.log,
                "next": "End your turn. Velocity will send the result when the command finishes. You can check status with `run_status` or cancel with `run_cancel` using this `run_id`."
            })
        })
}

pub fn request_run_status(port: u16, token: &str, arguments: Value) -> Result<Value, String> {
    let mut query: RunStatusRequest = if arguments.is_null() {
        RunStatusRequest::default()
    } else {
        serde_json::from_value(arguments).map_err(|error| error.to_string())?
    };
    query.run_id = query
        .run_id
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty());
    let request = Request {
        token: token.to_owned(),
        work: Work::Status(query),
    };
    submit::<Value>(port, &request, REQUEST_TIMEOUT)
        .map_err(|error| format!("Could not read run status: {error}"))?
}

pub fn request_run_cancel(port: u16, token: &str, arguments: Value) -> Result<Value, String> {
    let mut query: RunCancelRequest =
        serde_json::from_value(arguments).map_err(|error| error.to_string())?;
    query.run_id = query.run_id.trim().to_owned();
    if query.run_id.is_empty() {
        return Err("Provide a `run_id` to cancel".into());
    }
    let request = Request {
        token: token.to_owned(),
        work: Work::Cancel(query),
    };
    submit::<Value>(port, &request, REQUEST_TIMEOUT)
        .map_err(|error| format!("Could not cancel the run: {error}"))?
}

/// Sends `request` to the session and reads its answer, waiting up to `answer_timeout`.
pub(super) fn submit<Answer: DeserializeOwned>(
    port: u16,
    request: &Request,
    answer_timeout: Duration,
) -> io::Result<Result<Answer, String>> {
    let address = (Ipv4Addr::LOCALHOST, port).into();
    let mut socket = TcpStream::connect_timeout(&address, REQUEST_TIMEOUT)?;
    socket.set_read_timeout(Some(answer_timeout))?;
    socket.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    if line.len() as u64 > REQUEST_BYTES {
        return Err(io::Error::other("The request is too large"));
    }
    socket.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(socket).read_line(&mut reply)?;
    Ok(serde_json::from_str(&reply)?)
}
