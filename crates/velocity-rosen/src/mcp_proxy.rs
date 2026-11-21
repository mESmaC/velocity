//! Velocity's `rosen` MCP stdio proxy (#531).
//!
//! Wraps `rosen mcp` so every Rosen session gets:
//! - aligned `claim` guidance (`rosen claim --exclusive <paths>` for code paths, plain
//!   notices for registration lists and generated files);
//! - a non-blocking, parking `wait` tool that records Rosen path waits (`client.wait_paths`)
//!   and returns immediately so the agent can end its turn and let Velocity park and resume
//!   the thread once the waited paths are free.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use rosen_client::protocol::{
    ClaimFilter, ClaimKind, ClaimRequest, OverlapQuery, PathClaim, WaitOutcome,
};
use rosen_client::{Client, Endpoint};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::McpCommand;
use crate::server::program_folder;
use crate::{ProjectServer, RosenError};

pub const SERVER_SWITCH: &str = "--rosen-mcp";
pub const WAIT_TOOL: &str = "wait";

const INSTRUCTIONS_ADDENDUM: &str = " Claim code paths before editing with `claim` \
    (`exclusive: true`, matching `rosen claim --exclusive <paths>`) so the first claim wins; \
    plain notices (`exclusive: false`) are for registration lists and generated files, which \
    `--exclusive` refuses. Use `wait` to park until held paths are free: if it returns status \
    \"waiting\", end your turn without editing those paths and Velocity will park and resume \
    your thread once they are free.";

const CLAIM_ADDENDUM: &str = "\n\nUse `exclusive: true` (`rosen claim --exclusive <paths>`) \
    for code paths so the first claim wins. Use plain notices (`exclusive: false`) for \
    registration lists and generated files.";

const WAIT_DESCRIPTION: &str = "Wait until no other change claims the paths. Records a Rosen \
    path wait on this change and returns immediately without blocking. If any path is held by \
    another change, end your turn without editing those paths: Velocity parks your session and \
    resumes it once all waited paths are free (applying any rebase before you continue). \
    Returns immediately with status \"free\" when nobody holds the paths.";

/// Command specification to launch Velocity's `rosen` MCP proxy wrapping `rosen mcp`.
/// Returns `None` when the `rosen` binary itself is unavailable.
pub fn proxy_mcp_command(
    executable: &Path,
    root: &Path,
    environment: &[(String, String)],
) -> Option<McpCommand> {
    let inner = crate::mcp_command(root, environment)?;
    let mut arguments = vec![SERVER_SWITCH.to_owned()];
    arguments.extend(inner.arguments);
    Some(McpCommand {
        program: executable.to_owned(),
        arguments,
        environment: inner.environment,
    })
}

/// Active `ClaimKind::Wait` records currently held on `change`.
pub async fn active_waits(
    server: &ProjectServer,
    change: &str,
) -> Result<Vec<PathClaim>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let filter: ClaimFilter = serde_json::from_value(json!({"change": change}))
        .map_err(|error| RosenError::StatusData(error.to_string()))?;
    let claims = client.claim_list(filter).await?;
    Ok(claims
        .into_iter()
        .filter(|claim| claim.kind == ClaimKind::Wait)
        .collect())
}

/// Other live changes currently overlapping `paths`, as `(change_id, owner)` pairs.
pub async fn wait_holders(
    server: &ProjectServer,
    change: &str,
    paths: &[String],
) -> Result<Vec<(String, String)>, RosenError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let client = crate::connection::requests(server.endpoint()).await?;
    let query: OverlapQuery = serde_json::from_value(json!({"paths": paths}))
        .map_err(|error| RosenError::StatusData(error.to_string()))?;
    let answer = client.overlaps(query).await?;
    let mut holders = Vec::new();
    for overlap in answer.overlaps {
        let id = overlap.change.to_string();
        if id == change {
            continue;
        }
        let pair = (id, overlap.owner.to_string());
        if !holders.contains(&pair) {
            holders.push(pair);
        }
    }
    Ok(holders)
}

/// Serves the `rosen` MCP stdio proxy, forwarding non-wait methods to `rosen mcp`.
pub fn serve_stdio(rosen_args: &[String]) -> Result<(), RosenError> {
    let workspace = workspace_arg(rosen_args);
    let mut backend = ChildBackend::spawn(rosen_args, workspace)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let result = serve_streams(stdin.lock(), stdout.lock(), &mut backend);
    backend.shutdown();
    result
}

/// Transport-agnostic JSON-RPC loop used by both [`serve_stdio`] and unit tests.
pub fn serve_streams(
    input: impl BufRead,
    mut output: impl Write,
    backend: &mut impl ProxyBackend,
) -> Result<(), RosenError> {
    for line in input.lines() {
        let line = line.map_err(|source| io_error("read MCP request from", source))?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            write_json(
                &mut output,
                &json_rpc_error(Value::Null, -32700, "Parse error"),
            )?;
            continue;
        };
        if let Some(reply) = handle_request(&request, &line, backend)? {
            write_json(&mut output, &reply)?;
        }
    }
    Ok(())
}

pub trait ProxyBackend {
    fn forward_notification(&mut self, raw: &str) -> Result<(), RosenError>;
    fn forward_request(&mut self, id: &Value, raw: &str) -> Result<Value, RosenError>;
    fn wait_paths(
        &mut self,
        change: Option<&str>,
        paths: Vec<String>,
    ) -> Result<WaitOutcome, String>;
}

fn handle_request(
    request: &Value,
    raw: &str,
    backend: &mut impl ProxyBackend,
) -> Result<Option<Value>, RosenError> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let Some(id) = request.get("id") else {
        backend.forward_notification(raw)?;
        return Ok(None);
    };
    if method == "tools/call"
        && request
            .get("params")
            .and_then(|params| params.get("name"))
            .and_then(Value::as_str)
            == Some(WAIT_TOOL)
    {
        return Ok(Some(handle_wait_call(
            id,
            &request["params"]["arguments"],
            backend,
        )));
    }
    let mut response = backend.forward_request(id, raw)?;
    match method {
        "initialize" => augment_initialize(&mut response),
        "tools/list" => augment_tools_list(&mut response),
        _ => {}
    }
    Ok(Some(response))
}

#[derive(Deserialize)]
struct WaitArguments {
    #[serde(default)]
    change: Option<String>,
    paths: Vec<String>,
}

fn handle_wait_call(id: &Value, arguments: &Value, backend: &mut impl ProxyBackend) -> Value {
    let parsed: WaitArguments = match serde_json::from_value(arguments.clone()) {
        Ok(parsed) => parsed,
        Err(error) => return tool_result(id, &format!("Invalid wait arguments: {error}"), true),
    };
    if parsed.paths.is_empty() || parsed.paths.iter().any(|path| path.trim().is_empty()) {
        return tool_result(
            id,
            "wait requires at least one non-empty repository-relative path or glob",
            true,
        );
    }
    let change = parsed
        .change
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match backend.wait_paths(change, parsed.paths) {
        Ok(outcome) => {
            let body = format_wait_outcome(&outcome);
            tool_result(id, &body.to_string(), false)
        }
        Err(error) => tool_result(id, &error, true),
    }
}

fn format_wait_outcome(outcome: &WaitOutcome) -> Value {
    if outcome.waiting_on.is_empty() {
        json!({
            "schema": "rosen.wait/v1",
            "status": "free",
            "change": outcome.change,
            "free": outcome.free,
            "waiting_on": [],
            "cursor": outcome.cursor,
            "next": "All requested paths are free. Claim code paths with `claim` (`exclusive: true`) and continue."
        })
    } else {
        json!({
            "schema": "rosen.wait/v1",
            "status": "waiting",
            "change": outcome.change,
            "waiting_on": outcome.waiting_on,
            "free": outcome.free,
            "cursor": outcome.cursor,
            "next": "End your turn without editing the waited paths. Velocity will park your session and resume it once all waited paths are free."
        })
    }
}

pub fn augment_initialize(response: &mut Value) {
    let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };
    let current = result
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !current.contains("rosen claim --exclusive") {
        result.insert(
            "instructions".into(),
            Value::String(format!("{current}{INSTRUCTIONS_ADDENDUM}")),
        );
    }
}

pub fn augment_tools_list(response: &mut Value) {
    let Some(tools) = response
        .get_mut("result")
        .and_then(|result| result.get_mut("tools"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for tool in tools.iter_mut() {
        if tool.get("name").and_then(Value::as_str) == Some("claim")
            && let Some(description) = tool.get("description").and_then(Value::as_str)
            && !description.contains("rosen claim --exclusive")
        {
            tool["description"] = Value::String(format!("{description}{CLAIM_ADDENDUM}"));
        }
    }
    if !tools
        .iter()
        .any(|tool| tool.get("name").and_then(Value::as_str) == Some(WAIT_TOOL))
    {
        tools.push(wait_tool_definition());
    }
}

pub fn wait_tool_definition() -> Value {
    json!({
        "name": WAIT_TOOL,
        "description": WAIT_DESCRIPTION,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "change": {
                    "type": "string",
                    "description": "Change the wait belongs to: its ID or a unique prefix. Defaults to $ROSEN_CHANGE"
                },
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Repository-relative paths or globs: `?` and `*` match within one directory, a `**` segment any number of directories, and a trailing `/` everything under a directory"
                }
            },
            "required": ["paths"]
        }
    })
}

struct ChildBackend {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    workspace: Option<PathBuf>,
    runtime: tokio::runtime::Runtime,
}

impl ChildBackend {
    fn spawn(rosen_args: &[String], workspace: Option<PathBuf>) -> Result<Self, RosenError> {
        let program = resolve_rosen_binary()?;
        let mut command = crate::process::hidden_command(&program);
        command
            .args(rosen_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().map_err(|source| RosenError::Io {
            action: "start rosen mcp from",
            path: program.clone(),
            source,
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RosenError::StatusData("rosen mcp stdin pipe missing".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RosenError::StatusData("rosen mcp stdout pipe missing".into()))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|source| io_error("start async runtime for", source))?;
        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: BufReader::new(stdout),
            workspace,
            runtime,
        })
    }

    fn shutdown(&mut self) {
        self.stdin.take();
        let _ = self.child.wait();
    }

    fn write_child(&mut self, raw: &str) -> Result<(), RosenError> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(RosenError::Stopped);
        };
        writeln!(stdin, "{raw}").map_err(|source| io_error("write to rosen mcp", source))?;
        stdin
            .flush()
            .map_err(|source| io_error("flush rosen mcp", source))
    }

    fn resolve_change(&self, explicit: Option<&str>) -> Result<String, String> {
        if let Some(change) = explicit {
            return Ok(change.to_owned());
        }
        if let Ok(change) = std::env::var("ROSEN_CHANGE")
            && !change.trim().is_empty()
        {
            return Ok(change);
        }
        if let Some(workspace) = &self.workspace {
            let marker = workspace.join(".rosen").join("workspace.json");
            if let Ok(bytes) = std::fs::read(&marker)
                && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                && let Some(change) = value.get("change").and_then(Value::as_str)
                && !change.trim().is_empty()
            {
                return Ok(change.to_owned());
            }
        }
        Err("Missing change: pass `change` or set $ROSEN_CHANGE".into())
    }
}

impl ProxyBackend for ChildBackend {
    fn forward_notification(&mut self, raw: &str) -> Result<(), RosenError> {
        self.write_child(raw)
    }

    fn forward_request(&mut self, id: &Value, raw: &str) -> Result<Value, RosenError> {
        self.write_child(raw)?;
        let mut line = String::new();
        loop {
            line.clear();
            let read = self
                .stdout
                .read_line(&mut line)
                .map_err(|source| io_error("read from rosen mcp", source))?;
            if read == 0 {
                return Err(RosenError::Stopped);
            }
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&line)
                .map_err(|error| RosenError::StatusData(error.to_string()))?;
            if value.get("id") == Some(id) || value.get("id").is_some() {
                return Ok(value);
            }
        }
    }

    fn wait_paths(
        &mut self,
        change: Option<&str>,
        paths: Vec<String>,
    ) -> Result<WaitOutcome, String> {
        let change = self.resolve_change(change)?;
        let endpoint = match std::env::var("ROSEN_ENDPOINT") {
            Ok(value) if !value.trim().is_empty() => Endpoint::new(value),
            _ => return Err("Missing $ROSEN_ENDPOINT for Rosen path wait".into()),
        };
        let request: ClaimRequest = serde_json::from_value(json!({
            "change": change,
            "paths": paths,
        }))
        .map_err(|error| error.to_string())?;
        self.runtime.block_on(async move {
            let client = Client::connect(&endpoint)
                .await
                .map_err(|error| error.to_string())?;
            client
                .wait_paths(request)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

fn resolve_rosen_binary() -> Result<PathBuf, RosenError> {
    if let Some(explicit) = std::env::var_os("VELOCITY_ROSEN_EXE") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Ok(path);
        }
    }
    let folder = program_folder("rosen").ok_or(RosenError::MissingBinary)?;
    let path = folder.join(format!("rosen{}", std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        Ok(path)
    } else {
        Err(RosenError::MissingBinary)
    }
}

fn workspace_arg(arguments: &[String]) -> Option<PathBuf> {
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        if argument == "--workspace" {
            return iter.next().map(PathBuf::from);
        }
        if let Some(path) = argument.strip_prefix("--workspace=") {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn tool_result(id: &Value, text: &str, is_error: bool) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "isError": is_error,
            "content": [{ "type": "text", "text": text }]
        }
    })
}

fn json_rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn write_json(output: &mut impl Write, value: &Value) -> Result<(), RosenError> {
    serde_json::to_writer(&mut *output, value)
        .map_err(|error| RosenError::StatusData(error.to_string()))?;
    writeln!(output).map_err(|source| io_error("write MCP reply to", source))?;
    output
        .flush()
        .map_err(|source| io_error("flush MCP reply to", source))
}

fn io_error(action: &'static str, source: io::Error) -> RosenError {
    RosenError::Io {
        action,
        path: PathBuf::from("stdio"),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct FakeBackend {
        waits: Vec<(Option<String>, Vec<String>)>,
        outcome: Result<WaitOutcome, String>,
    }

    impl ProxyBackend for FakeBackend {
        fn forward_notification(&mut self, _raw: &str) -> Result<(), RosenError> {
            Ok(())
        }

        fn forward_request(&mut self, id: &Value, raw: &str) -> Result<Value, RosenError> {
            let request: Value = serde_json::from_str(raw)
                .map_err(|error| RosenError::StatusData(error.to_string()))?;
            match request["method"].as_str() {
                Some("initialize") => Ok(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": { "tools": {} },
                        "instructions": "Rosen is this project's version control."
                    }
                })),
                Some("tools/list") => Ok(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": [{
                            "name": "claim",
                            "description": "Notices always succeed. With --exclusive, the first claim wins."
                        }]
                    }
                })),
                _ => Ok(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            }
        }

        fn wait_paths(
            &mut self,
            change: Option<&str>,
            paths: Vec<String>,
        ) -> Result<WaitOutcome, String> {
            self.waits.push((change.map(str::to_owned), paths));
            self.outcome.clone()
        }
    }

    // Risk (#531): the Rosen MCP proxy must expose a parking `wait` tool and align `claim`
    // instructions with `rosen claim --exclusive <paths>` without blocking the MCP call.
    #[test]
    fn proxy_exposes_parking_wait_and_exclusive_claim_guidance()
    -> Result<(), Box<dyn std::error::Error>> {
        let waiting: WaitOutcome = serde_json::from_value(json!({
            "change": "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
            "waiting_on": [{
                "requested": "src/lib.rs",
                "held": {
                    "change": "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm",
                    "owner": "other-agent",
                    "path": "src/lib.rs",
                    "kind": "claim",
                    "exclusive": true,
                    "since": 1
                }
            }],
            "free": [],
            "cursor": 42
        }))?;
        let mut backend = FakeBackend {
            waits: Vec::new(),
            outcome: Ok(waiting),
        };
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"wait","arguments":{"paths":["src/lib.rs"]}}}).to_string(),
        ]
        .join("\n");
        let mut out = Vec::new();
        serve_streams(Cursor::new(requests), &mut out, &mut backend)?;
        let lines: Vec<Value> = String::from_utf8(out)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(lines.len(), 3);
        let instructions = lines[0]["result"]["instructions"].as_str().unwrap_or("");
        assert!(instructions.contains("rosen claim --exclusive <paths>"));
        let tools = lines[1]["result"]["tools"]
            .as_array()
            .ok_or("missing tools")?;
        let claim = tools
            .iter()
            .find(|t| t["name"] == "claim")
            .ok_or("missing claim")?;
        assert!(
            claim["description"]
                .as_str()
                .unwrap_or("")
                .contains("rosen claim --exclusive <paths>")
        );
        assert!(tools.iter().any(|t| t["name"] == "wait"));
        assert_eq!(lines[2]["result"]["isError"], json!(false));
        let wait_text = lines[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("");
        let wait_reply: Value = serde_json::from_str(wait_text)?;
        assert_eq!(wait_reply["status"], json!("waiting"));
        assert_eq!(backend.waits.len(), 1);
        assert_eq!(backend.waits[0].1, vec!["src/lib.rs".to_owned()]);
        Ok(())
    }
}
