//! The stdio MCP framing Velocity's own duty servers share (F7 scouts and PR duty): one
//! JSON-RPC request per line in, one response per line out, and the four methods an agent
//! needs (`initialize`, `ping`, `tools/list`, `tools/call`).

use std::fs::OpenOptions;
use std::io::{BufRead, Write};
use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// One duty MCP server: its name, what it tells the agent, its tools and how it runs them.
pub(crate) trait ToolServer {
    fn name(&self) -> &'static str;
    fn instructions(&self) -> String;
    fn tools(&self) -> Vec<Value>;
    /// Runs tool `name`. An error is shown to the agent as the tool's failed result.
    fn call(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String>;
}

/// Answers JSON-RPC requests, one per line, until the agent closes the connection.
pub(crate) fn serve(
    server: &mut impl ToolServer,
    input: impl BufRead,
    mut output: impl Write,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => respond(server, &request),
            Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            writeln!(output)?;
            output.flush()?;
        }
    }
    Ok(())
}

fn respond(server: &mut impl ToolServer, request: &Value) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
        return Some(failure(id, -32600, "Invalid request"));
    }
    // A notification (no id) gets no answer.
    request.get("id")?;
    let result = match method.unwrap_or_default() {
        "initialize" => json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": server.name(), "version": env!("CARGO_PKG_VERSION")},
            "instructions": server.instructions(),
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": server.tools()}),
        "tools/call" => {
            let parameters = &request["params"];
            let name = parameters.get("name").and_then(Value::as_str);
            let arguments = parameters.get("arguments").cloned().unwrap_or(json!({}));
            match server.call(name, arguments) {
                Ok(value) => json!({"content": [{"type": "text", "text": value.to_string()}]}),
                Err(error) => {
                    json!({"isError": true, "content": [{"type": "text", "text": error}]})
                }
            }
        }
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn failure(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Appends one report line for Velocity to read. One short append per outcome; the reader
/// takes whole lines only.
pub(crate) fn append_report(path: &Path, report: &impl Serialize) -> std::io::Result<()> {
    let mut line = serde_json::to_string(report)?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

/// Reads the report lines appended since `offset` bytes, and the offset after the last
/// complete line, so a line the server is still writing is read next time.
pub(crate) fn read_reports<T: DeserializeOwned>(
    path: &Path,
    offset: u64,
) -> std::io::Result<(Vec<T>, u64)> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), offset));
        }
        Err(error) => return Err(error),
    };
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let unread = &bytes[start..];
    let complete = unread
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |last| last + 1);
    let reports = unread[..complete]
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        // A line this build can't read is skipped rather than stalling the feed.
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect();
    Ok((reports, offset + complete as u64))
}
