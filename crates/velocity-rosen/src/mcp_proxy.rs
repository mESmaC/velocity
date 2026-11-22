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

use rosen_client::protocol::{ClaimFilter, ClaimKind, ClaimRequest, PathClaim, WaitOutcome};
use rosen_client::{Client, Endpoint};
use rosen_core::PathPattern;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::McpCommand;
use crate::mcp_filter::{
    FilterPolicy, LAZY_FLAG, LOADER_TOOL, ROLE_FLAG, enable_list_changed, extract_occasional_tools,
    filter_tools_list, parse_policy_args, refusal_text,
};
use crate::server::program_folder;
use crate::{ProjectServer, RosenError};
use velocity_core::tool_policy::{McpToolServer, ThreadRole, tool_access};

pub const SERVER_SWITCH: &str = "--rosen-mcp";
pub const WAIT_TOOL: &str = "wait";
pub const COMMIT_TOOL: &str = "commit";

const INSTRUCTIONS_ADDENDUM: &str = " Claim code paths before editing with `claim` \
    (`exclusive: true`, matching `rosen claim --exclusive <paths>`) so the first claim wins; \
    plain notices (`exclusive: false`) are for registration lists and generated files, which \
    `--exclusive` refuses. Use `wait` to park until held paths are free: if it returns status \
    \"waiting\", end your turn without editing those paths and Velocity will park and resume \
    your thread once they are free. Run `preflight` from `velocity-run` before calling `commit`.";

const CLAIM_ADDENDUM: &str = "\n\nUse `exclusive: true` (`rosen claim --exclusive <paths>`) \
    for code paths so the first claim wins. Use plain notices (`exclusive: false`) for \
    registration lists and generated files.";

const COMMIT_ADDENDUM: &str = "\n\nVelocity requires a passing `preflight` run on the current \
    workspace snapshot before naming a change.";

const WAIT_DESCRIPTION: &str = "Wait until no other change claims the paths. Records a Rosen \
    path wait on this change and returns immediately without blocking. If any path is held by \
    another change, end your turn without editing those paths: Velocity parks your session and \
    resumes it once all waited paths are free (applying any rebase before you continue). \
    Returns immediately with status \"free\" when nobody holds the paths.";

/// Command specification to launch Velocity's `rosen` MCP proxy wrapping `rosen mcp`
/// for `ThreadRole::Builder`. Returns `None` when the `rosen` binary itself is unavailable.
pub fn proxy_mcp_command(
    executable: &Path,
    root: &Path,
    environment: &[(String, String)],
) -> Option<McpCommand> {
    proxy_mcp_command_for_role(
        executable,
        Some(root),
        environment,
        ThreadRole::Builder,
        false,
    )
}

/// Command specification to launch Velocity's role-filtered `rosen` MCP proxy (#531, #533).
/// `workspace` is `None` for read-only question threads that have no workspace.
pub fn proxy_mcp_command_for_role(
    executable: &Path,
    workspace: Option<&Path>,
    environment: &[(String, String)],
    role: ThreadRole,
    lazy: bool,
) -> Option<McpCommand> {
    let (inner_args, inner_env) = match workspace {
        Some(root) => {
            let inner = crate::mcp_command(root, environment)?;
            (inner.arguments, inner.environment)
        }
        None => {
            let real_rosen =
                program_folder("rosen")?.join(format!("rosen{}", std::env::consts::EXE_SUFFIX));
            if !real_rosen.is_file() {
                return None;
            }
            let env = environment
                .iter()
                .filter(|(name, _)| name.starts_with("ROSEN_"))
                .cloned()
                .collect();
            (vec!["mcp".to_owned()], env)
        }
    };
    let mut arguments = vec![
        SERVER_SWITCH.to_owned(),
        ROLE_FLAG.to_owned(),
        role.as_str().to_owned(),
    ];
    if lazy {
        arguments.push(LAZY_FLAG.to_owned());
    }
    arguments.extend(inner_args);
    Some(McpCommand {
        program: executable.to_owned(),
        arguments,
        environment: inner_env,
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

/// Other live changes holding `ClaimKind::Claim` records overlapping `paths`, as
/// `(change_id, owner)` pairs. Shares, waits, and unclaimed snapshot edits do not block
/// Rosen waits and are excluded (#674).
pub async fn wait_holders(
    server: &ProjectServer,
    change: &str,
    paths: &[String],
) -> Result<Vec<(String, String)>, RosenError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let client = crate::connection::requests(server.endpoint()).await?;
    let claims = client.claim_list(ClaimFilter::default()).await?;
    blocking_wait_holders(change, paths, &claims)
}

/// Other live changes holding `ClaimKind::Claim` records that currently block `change`'s
/// active `ClaimKind::Wait` records, derived from one live `claim_list` snapshot (#674).
pub async fn active_wait_holders(
    server: &ProjectServer,
    change: &str,
) -> Result<Vec<(String, String)>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let claims = client.claim_list(ClaimFilter::default()).await?;
    active_blocking_wait_holders(change, &claims)
}

/// Pure blocker selection matching Rosen's wait transaction: only live `ClaimKind::Claim`
/// records on other changes block `paths`; shares, waits, and unclaimed snapshot edits do
/// not (#674). Preserves claim order and deduplicates by change ID.
pub fn blocking_wait_holders(
    change: &str,
    paths: &[String],
    claims: &[PathClaim],
) -> Result<Vec<(String, String)>, RosenError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let requested: Vec<PathPattern> = paths
        .iter()
        .map(|path| PathPattern::new(path))
        .collect::<Result<_, _>>()
        .map_err(|error| RosenError::StatusData(error.to_string()))?;
    let mut holders = Vec::new();
    for claim in claims {
        if claim.kind != ClaimKind::Claim {
            continue;
        }
        let id = claim.change.to_string();
        if id == change {
            continue;
        }
        let held = PathPattern::new(&claim.path)
            .map_err(|error| RosenError::StatusData(error.to_string()))?;
        if requested.iter().any(|pattern| held.overlaps(pattern))
            && !holders.iter().any(|(existing, _)| existing == &id)
        {
            holders.push((id, claim.owner.to_string()));
        }
    }
    Ok(holders)
}

/// Pure blocker selection for `change`'s active `ClaimKind::Wait` records in `claims` (#674).
/// Returns empty when `change` has no active wait records.
pub fn active_blocking_wait_holders(
    change: &str,
    claims: &[PathClaim],
) -> Result<Vec<(String, String)>, RosenError> {
    let waited_paths: Vec<String> = claims
        .iter()
        .filter(|claim| claim.kind == ClaimKind::Wait && claim.change.to_string() == change)
        .map(|claim| claim.path.clone())
        .collect();
    blocking_wait_holders(change, &waited_paths, claims)
}

/// Serves the `rosen` MCP stdio proxy, forwarding non-wait methods to `rosen mcp`.
pub fn serve_stdio(raw_args: &[String]) -> Result<(), RosenError> {
    let (policy, rosen_args) = parse_policy_args(raw_args);
    let workspace = workspace_arg(&rosen_args);
    let mut backend = ChildBackend::spawn(&rosen_args, workspace)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let result = serve_streams_with_policy(stdin.lock(), stdout.lock(), &mut backend, policy);
    backend.shutdown();
    result
}

/// Transport-agnostic JSON-RPC loop used by both [`serve_stdio`] and unit tests.
pub fn serve_streams(
    input: impl BufRead,
    output: impl Write,
    backend: &mut impl ProxyBackend,
) -> Result<(), RosenError> {
    serve_streams_with_policy(input, output, backend, None)
}

/// Transport-agnostic JSON-RPC loop applying optional per-role filtering and lazy loading (#533).
pub fn serve_streams_with_policy(
    input: impl BufRead,
    mut output: impl Write,
    backend: &mut impl ProxyBackend,
    policy: Option<FilterPolicy>,
) -> Result<(), RosenError> {
    let mut occasional_loaded = false;
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
        let (reply, list_changed) =
            handle_request(&request, &line, backend, policy, &mut occasional_loaded)?;
        if let Some(reply) = reply {
            write_json(&mut output, &reply)?;
        }
        if list_changed {
            write_json(
                &mut output,
                &json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
            )?;
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
    fn check_commit_preflight(&mut self, _workspace: Option<&Path>) -> Result<(), String> {
        Ok(())
    }
}

fn handle_request(
    request: &Value,
    raw: &str,
    backend: &mut impl ProxyBackend,
    policy: Option<FilterPolicy>,
    occasional_loaded: &mut bool,
) -> Result<(Option<Value>, bool), RosenError> {
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let Some(id) = request.get("id") else {
        backend.forward_notification(raw)?;
        return Ok((None, false));
    };
    if method == "tools/call" {
        return handle_tool_call(request, raw, id, backend, policy, occasional_loaded);
    }
    let mut response = backend.forward_request(id, raw)?;
    match method {
        "initialize" => {
            augment_initialize(&mut response);
            if policy.is_some_and(|item| item.lazy) {
                enable_list_changed(&mut response);
            }
        }
        "tools/list" => {
            augment_tools_list(&mut response);
            if let Some(policy) = policy {
                filter_tools_list(&mut response, policy, *occasional_loaded);
            }
        }
        _ => {}
    }
    Ok((Some(response), false))
}

fn handle_tool_call(
    request: &Value,
    raw: &str,
    id: &Value,
    backend: &mut impl ProxyBackend,
    policy: Option<FilterPolicy>,
    occasional_loaded: &mut bool,
) -> Result<(Option<Value>, bool), RosenError> {
    let name = request
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(policy) = policy {
        if policy.lazy && name == LOADER_TOOL {
            return handle_loader_call(
                id,
                &request["params"]["arguments"],
                backend,
                policy.role,
                occasional_loaded,
            );
        }
        if !tool_access(policy.role, McpToolServer::Rosen, name).is_allowed() {
            return Ok((
                Some(tool_result(id, &refusal_text(policy.role, name), true)),
                false,
            ));
        }
    }
    if name == WAIT_TOOL {
        return Ok((
            Some(handle_wait_call(
                id,
                &request["params"]["arguments"],
                backend,
            )),
            false,
        ));
    }
    if name == COMMIT_TOOL
        && let Err(refusal) = check_commit_call(&request["params"]["arguments"], backend)
    {
        return Ok((Some(tool_result(id, &refusal, true)), false));
    }
    Ok((Some(backend.forward_request(id, raw)?), false))
}

fn handle_loader_call(
    id: &Value,
    arguments: &Value,
    backend: &mut impl ProxyBackend,
    role: ThreadRole,
    occasional_loaded: &mut bool,
) -> Result<(Option<Value>, bool), RosenError> {
    let action = arguments["action"].as_str().unwrap_or("list");
    match action {
        "list" => {
            let list_id = json!("__velocity_tools_list");
            let list_raw =
                json!({"jsonrpc": "2.0", "id": list_id, "method": "tools/list", "params": {}})
                    .to_string();
            let mut list_reply = backend.forward_request(&list_id, &list_raw)?;
            augment_tools_list(&mut list_reply);
            let schemas = extract_occasional_tools(&list_reply, role);
            let changed = !*occasional_loaded;
            *occasional_loaded = true;
            let body = json!({"loaded": true, "tools": schemas}).to_string();
            Ok((Some(tool_result(id, &body, false)), changed))
        }
        "call" => {
            let tool = arguments["tool"].as_str().unwrap_or("");
            if !tool_access(role, McpToolServer::Rosen, tool).is_allowed() {
                return Ok((
                    Some(tool_result(id, &refusal_text(role, tool), true)),
                    false,
                ));
            }
            let tool_args = arguments
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if tool == WAIT_TOOL {
                return Ok((Some(handle_wait_call(id, &tool_args, backend)), false));
            }
            if tool == COMMIT_TOOL
                && let Err(refusal) = check_commit_call(&tool_args, backend)
            {
                return Ok((Some(tool_result(id, &refusal, true)), false));
            }
            let forwarded = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": { "name": tool, "arguments": tool_args }
            })
            .to_string();
            Ok((Some(backend.forward_request(id, &forwarded)?), false))
        }
        other => Ok((
            Some(tool_result(
                id,
                &format!("Unsupported action `{other}`; use `list` or `call`."),
                true,
            )),
            false,
        )),
    }
}

fn check_commit_call(arguments: &Value, backend: &mut impl ProxyBackend) -> Result<(), String> {
    let workspace = arguments
        .get("workspace")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    backend.check_commit_preflight(workspace.as_deref())
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
        if tool.get("name").and_then(Value::as_str) == Some(COMMIT_TOOL)
            && let Some(description) = tool.get("description").and_then(Value::as_str)
            && !description.contains("passing `preflight`")
        {
            tool["description"] = Value::String(format!("{description}{COMMIT_ADDENDUM}"));
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

    fn check_commit_preflight(&mut self, workspace: Option<&Path>) -> Result<(), String> {
        let endpoint = match std::env::var("ROSEN_ENDPOINT") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(()),
        };
        let root = match workspace
            .map(Path::to_path_buf)
            .or_else(|| self.workspace.clone())
        {
            Some(path) => path,
            None => std::env::current_dir().map_err(|error| error.to_string())?,
        };
        self.runtime.block_on(async move {
            match crate::preflight::verify_workspace_for_commit(&endpoint, &root).await {
                Ok(result) => result,
                Err(error) => Err(format!(
                    "Could not verify preflight before naming the change: {error}"
                )),
            }
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

    // Risk (#533): a builder thread's Rosen MCP server must hide and refuse destructive or
    // reviewer-only tools (`workspace_delete`, `change_abandon`, `review_mark`, `review_landed`)
    // while keeping occasional tools (including `wait`) behind `rosen_tools` when lazy.
    #[test]
    fn builder_policy_filters_forbidden_and_defers_occasional_tools()
    -> Result<(), Box<dyn std::error::Error>> {
        struct RichBackend {
            waits: Vec<Vec<String>>,
            forwarded: Vec<String>,
        }
        impl ProxyBackend for RichBackend {
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
                        "result": { "capabilities": { "tools": {} }, "instructions": "Rosen." }
                    })),
                    Some("tools/list") => Ok(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": [
                                {"name": "status", "description": "status"},
                                {"name": "commit", "description": "commit"},
                                {"name": "claim", "description": "claim"},
                                {"name": "review_list", "description": "review_list"},
                                {"name": "workspace_delete", "description": "workspace_delete"},
                                {"name": "change_abandon", "description": "change_abandon"},
                                {"name": "review_mark", "description": "review_mark"},
                                {"name": "review_landed", "description": "review_landed"}
                            ]
                        }
                    })),
                    Some("tools/call") => {
                        let name = request["params"]["name"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned();
                        self.forwarded.push(name.clone());
                        Ok(tool_result(id, &name, false))
                    }
                    _ => Ok(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
                }
            }
            fn wait_paths(
                &mut self,
                _change: Option<&str>,
                paths: Vec<String>,
            ) -> Result<WaitOutcome, String> {
                self.waits.push(paths.clone());
                serde_json::from_value(json!({
                    "change": "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk",
                    "waiting_on": [],
                    "free": paths,
                    "cursor": 1
                }))
                .map_err(|error| error.to_string())
            }
        }
        let mut backend = RichBackend {
            waits: Vec::new(),
            forwarded: Vec::new(),
        };
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}).to_string(),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"workspace_delete","arguments":{}}}).to_string(),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"rosen_tools","arguments":{"action":"call","tool":"wait","arguments":{"paths":["src/lib.rs"]}}}}).to_string(),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"rosen_tools","arguments":{"action":"list"}}}).to_string(),
            json!({"jsonrpc":"2.0","id":6,"method":"tools/list","params":{}}).to_string(),
        ]
        .join("\n");
        let mut out = Vec::new();
        serve_streams_with_policy(
            Cursor::new(requests),
            &mut out,
            &mut backend,
            Some(FilterPolicy {
                role: ThreadRole::Builder,
                lazy: true,
            }),
        )?;
        let lines: Vec<Value> = String::from_utf8(out)?
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()?;
        assert_eq!(
            lines[0]["result"]["capabilities"]["tools"]["listChanged"],
            json!(true)
        );
        let initial: Vec<&str> = lines[1]["result"]["tools"]
            .as_array()
            .ok_or("missing tools")?
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(initial.contains(&"status"));
        assert!(initial.contains(&"commit"));
        assert!(initial.contains(&LOADER_TOOL));
        for hidden in [
            "workspace_delete",
            "change_abandon",
            "review_mark",
            "review_landed",
            "review_list",
        ] {
            assert!(
                !initial.contains(&hidden),
                "{hidden} should not be in initial builder list"
            );
        }
        assert_eq!(lines[2]["result"]["isError"], json!(true));
        assert!(backend.forwarded.is_empty());
        assert_eq!(lines[3]["result"]["isError"], json!(false));
        assert_eq!(backend.waits, vec![vec!["src/lib.rs".to_owned()]]);
        assert_eq!(
            lines[5]["method"],
            json!("notifications/tools/list_changed")
        );
        let reloaded: Vec<&str> = lines[6]["result"]["tools"]
            .as_array()
            .ok_or("missing reloaded tools")?
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(reloaded.contains(&"review_list"));
        assert!(reloaded.contains(&WAIT_TOOL));
        assert!(!reloaded.contains(&"workspace_delete"));
        Ok(())
    }

    // Risk (#560): calling the Rosen MCP `commit` tool without a passing preflight must be
    // refused before reaching `rosen mcp` and report what failed.
    #[test]
    fn commit_tool_enforces_preflight_gate() -> Result<(), Box<dyn std::error::Error>> {
        struct CommitBackend {
            gate: Result<(), String>,
            forwarded: Vec<String>,
        }
        impl ProxyBackend for CommitBackend {
            fn forward_notification(&mut self, _raw: &str) -> Result<(), RosenError> {
                Ok(())
            }
            fn forward_request(&mut self, id: &Value, raw: &str) -> Result<Value, RosenError> {
                let request: Value = serde_json::from_str(raw)
                    .map_err(|error| RosenError::StatusData(error.to_string()))?;
                let name = request["params"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.forwarded.push(name.clone());
                Ok(tool_result(id, &name, false))
            }
            fn wait_paths(
                &mut self,
                _change: Option<&str>,
                _paths: Vec<String>,
            ) -> Result<WaitOutcome, String> {
                Err("unused".into())
            }
            fn check_commit_preflight(&mut self, _workspace: Option<&Path>) -> Result<(), String> {
                self.gate.clone()
            }
        }
        let commit_call = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "commit", "arguments": { "message": "Ship it (#560)" } }
        })
        .to_string();

        let mut refused_backend = CommitBackend {
            gate: Err("Cannot name change: clippy failed".into()),
            forwarded: Vec::new(),
        };
        let mut out = Vec::new();
        serve_streams(Cursor::new(&commit_call), &mut out, &mut refused_backend)?;
        let reply: Value = serde_json::from_str(String::from_utf8(out)?.trim())?;
        assert_eq!(reply["result"]["isError"], json!(true));
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("clippy failed"))
        );
        assert!(refused_backend.forwarded.is_empty());

        let mut passed_backend = CommitBackend {
            gate: Ok(()),
            forwarded: Vec::new(),
        };
        let mut out = Vec::new();
        serve_streams(Cursor::new(&commit_call), &mut out, &mut passed_backend)?;
        let reply: Value = serde_json::from_str(String::from_utf8(out)?.trim())?;
        assert_eq!(reply["result"]["isError"], json!(false));
        assert_eq!(passed_backend.forwarded, vec!["commit".to_owned()]);
        Ok(())
    }

    fn path_claim(
        change: &str,
        owner: &str,
        path: &str,
        kind: &str,
        exclusive: bool,
        since: i64,
    ) -> Result<PathClaim, serde_json::Error> {
        serde_json::from_value(json!({
            "change": change,
            "owner": owner,
            "path": path,
            "kind": kind,
            "exclusive": exclusive,
            "since": since
        }))
    }

    // Risk (#674): Rosen wait blockers must include only live `ClaimKind::Claim` records
    // overlapping active waits, ignoring incidental shares, other waits, and unclaimed snapshot
    // edits, while preserving every distinct blocking change.
    #[test]
    fn active_wait_holders_ignore_shares_and_report_all_claim_blockers()
    -> Result<(), Box<dyn std::error::Error>> {
        let sharer = "kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk";
        let blocker_a = "mmmmmmmmmmmmmmmmmmmmmmmmmmmmmmmm";
        let blocker_b = "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn";
        let stale_holder = "pppppppppppppppppppppppppppppppp";
        let other_waiter = "qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        let waiter = "rrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr";

        // Incidental share sorts before the real blocker; unclaimed snapshot edits have no
        // Claim record at all. Neither may appear as a wait blocker.
        let claims = vec![
            path_claim(sharer, "sharer-1", "src/code.rs", "share", false, 1)?,
            path_claim(other_waiter, "waiter-2", "src/code.rs", "wait", false, 2)?,
            path_claim(waiter, "waiter-1", "src/other.rs", "claim", true, 3)?,
            path_claim(blocker_a, "blocker-1", "src/**", "claim", true, 4)?,
            path_claim(blocker_a, "blocker-1", "src/code.rs", "claim", true, 5)?,
            path_claim(stale_holder, "stale-1", "src/released.rs", "claim", true, 6)?,
            path_claim(waiter, "waiter-1", "src/code.rs", "wait", false, 7)?,
        ];
        assert_eq!(
            active_blocking_wait_holders(waiter, &claims)?,
            vec![(blocker_a.to_owned(), "blocker-1".to_owned())]
        );

        // Two distinct changes blocking either the same path or different waited paths both
        // appear so F16 stacking keeps waiting while any other holder blocks a path.
        let mut multi_claims = claims.clone();
        multi_claims.push(path_claim(
            blocker_b,
            "blocker-2",
            "src/code.rs",
            "claim",
            false,
            8,
        )?);
        let expected_two = vec![
            (blocker_a.to_owned(), "blocker-1".to_owned()),
            (blocker_b.to_owned(), "blocker-2".to_owned()),
        ];
        assert_eq!(
            active_blocking_wait_holders(waiter, &multi_claims)?,
            expected_two
        );

        let mut split_path_claims = claims;
        split_path_claims.push(path_claim(
            blocker_b,
            "blocker-2",
            "src/extra.rs",
            "claim",
            true,
            8,
        )?);
        split_path_claims.push(path_claim(
            waiter,
            "waiter-1",
            "src/extra.rs",
            "wait",
            false,
            9,
        )?);
        assert_eq!(
            active_blocking_wait_holders(waiter, &split_path_claims)?,
            expected_two
        );

        // Once the waiter's own `Wait` record is gone, active_blocking_wait_holders is empty
        // even though other changes still hold claims on the old path.
        let no_active_wait: Vec<PathClaim> = multi_claims
            .into_iter()
            .filter(|claim| !(claim.change.to_string() == waiter && claim.kind == ClaimKind::Wait))
            .collect();
        assert!(active_blocking_wait_holders(waiter, &no_active_wait)?.is_empty());
        assert_eq!(
            blocking_wait_holders(waiter, &["src/code.rs".to_owned()], &no_active_wait)?,
            expected_two
        );
        Ok(())
    }
}
