//! The stdio MCP subprocess hands requests to its owning ACP session over loopback.

use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const QUEUED_RUN_SWITCH: &str = "--queued-run-mcp";
pub const QUEUED_RUN_SERVER: &str = "velocity-run";
pub const QUEUED_RUN_TOOL: &str = "queue_run";
pub const QUEUED_RUN_TOKEN_ENV: &str = "VELOCITY_QUEUED_RUN_TOKEN";
pub const QUEUED_RUN_PORT_ENV: &str = "VELOCITY_QUEUED_RUN_PORT";
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const REQUEST_BYTES: u64 = 128 * 1024;

pub(super) const INSTRUCTIONS: &str = "Use `queue_run` from `velocity-run` for commands that \
    can take more than a minute, including `cargo check -p <crate>` and Lean checks. \
    Pass the program and its arguments separately. Agents use cargo fmt and UI type-check \
    and lint. The integration build covers Rust compilation, clippy and tests. After a \
    command queues, end your turn. Velocity sends the result and log path in your next prompt.";

#[derive(Deserialize, Serialize)]
pub(super) struct Request {
    pub token: String,
    pub command: RunCommand,
}

#[derive(Deserialize, Serialize)]
pub(super) struct RunCommand {
    pub program: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub directory: Option<PathBuf>,
    #[serde(default)]
    class: RunClass,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum RunClass {
    #[default]
    Build,
    Landing,
    Verify,
}

impl RunCommand {
    pub fn class(&self) -> &'static str {
        match self.class {
            RunClass::Build => "build",
            RunClass::Landing => "landing",
            RunClass::Verify => "verify",
        }
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

pub fn request_queued_run(port: u16, token: &str, arguments: Value) -> Result<Value, String> {
    let command: RunCommand =
        serde_json::from_value(arguments).map_err(|error| error.to_string())?;
    if command.program.trim().is_empty() {
        return Err("Provide a program to run".into());
    }
    let request = Request {
        token: token.to_owned(),
        command,
    };
    submit(port, &request)
        .map_err(|error| format!("Could not queue the command: {error}"))?
        .map(|path| json!({"queued": true, "log": path, "next": "End your turn. Velocity will send the result when the command finishes."}))
}

fn submit(port: u16, request: &Request) -> io::Result<Result<PathBuf, String>> {
    let address = (Ipv4Addr::LOCALHOST, port).into();
    let mut socket = TcpStream::connect_timeout(&address, REQUEST_TIMEOUT)?;
    socket.set_read_timeout(Some(REQUEST_TIMEOUT))?;
    socket.set_write_timeout(Some(REQUEST_TIMEOUT))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    if line.len() as u64 > REQUEST_BYTES {
        return Err(io::Error::other("Command arguments are too large"));
    }
    socket.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(socket).read_line(&mut reply)?;
    Ok(serde_json::from_str(&reply)?)
}
