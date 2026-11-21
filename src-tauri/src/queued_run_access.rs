use std::io::{self, BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use velocity_acp::client::{
    PREFLIGHT_TOOL, QUEUED_RUN_SERVER, QUEUED_RUN_SWITCH, QUEUED_RUN_TOKEN_ENV, QUEUED_RUN_TOOL,
    RUN_CANCEL_TOOL, RUN_STATUS_TOOL, WORKFLOW_RUN_TOOL, preflight_tool, queued_run_tool,
    request_preflight, request_queued_run, request_run_cancel, request_run_status,
    request_workflow_run, run_cancel_tool, run_status_tool, workflow_run_tool,
};
use velocity_core::text_budget::tool_answer;
use velocity_core::tool_policy::{McpToolServer, ThreadRole, ToolAccess, tool_access};
use velocity_rosen::mcp_latency::LatencyRecorder;

const LOADER_TOOL: &str = "run_tools";

pub fn serve() -> anyhow::Result<bool> {
    let mut arguments = std::env::args_os().skip(1);
    if arguments
        .next()
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        != Some(QUEUED_RUN_SWITCH)
    {
        return Ok(false);
    }
    let port = arguments
        .next()
        .and_then(|port| port.into_string().ok())
        .ok_or_else(|| anyhow::anyhow!("Missing queued run port"))?
        .parse()?;
    let (role, lazy) = parse_flags(arguments.collect())?;
    let token = std::env::var(QUEUED_RUN_TOKEN_ENV)?;
    let latency = LatencyRecorder::open(QUEUED_RUN_SERVER, None);
    let mut server = Server {
        port,
        token,
        latency,
        role,
        lazy,
        occasional_loaded: false,
    };
    serve_stdio(&mut server, io::stdin().lock(), io::stdout().lock())?;
    Ok(true)
}

fn parse_flags(args: Vec<std::ffi::OsString>) -> anyhow::Result<(Option<ThreadRole>, bool)> {
    let mut role = None;
    let mut lazy = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.to_str() {
            Some("--role") => {
                let value = args
                    .get(index + 1)
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing value for --role"))?;
                role = Some(
                    ThreadRole::parse(value)
                        .ok_or_else(|| anyhow::anyhow!("Unknown thread role: {value}"))?,
                );
                index += 2;
            }
            Some("--lazy") => {
                lazy = true;
                index += 1;
            }
            _ => index += 1,
        }
    }
    Ok((role, lazy))
}

struct Server {
    port: u16,
    token: String,
    latency: LatencyRecorder,
    role: Option<ThreadRole>,
    lazy: bool,
    occasional_loaded: bool,
}

impl Server {
    fn instructions(&self) -> &'static str {
        "Run `preflight` before naming a change, queue checks and builds with `queue_run`, \
         inspect or cancel runs with `run_status`/`run_cancel`, or request an owner-allowed \
         GitHub workflow run with `workflow_run`, then end your turn to receive the result."
    }

    fn all_tools() -> Vec<Value> {
        vec![
            queued_run_tool(),
            run_status_tool(),
            run_cancel_tool(),
            workflow_run_tool(),
            preflight_tool(),
        ]
    }

    fn tools(&self) -> Vec<Value> {
        let all = Self::all_tools();
        let Some(role) = self.role else {
            return all;
        };
        let mut occasional_names = Vec::new();
        let mut listed = Vec::new();
        for entry in all {
            let Some(name) = entry["name"].as_str() else {
                continue;
            };
            let access = tool_access(role, McpToolServer::Run, name);
            if access == ToolAccess::Occasional {
                occasional_names.push(name.to_owned());
            }
            if access.is_listed(self.lazy, self.occasional_loaded) {
                listed.push(entry);
            }
        }
        if self.lazy && !self.occasional_loaded && !occasional_names.is_empty() {
            listed.push(json!({
                "name": LOADER_TOOL,
                "description": format!(
                    "Load or call occasional velocity-run tools on demand ({}). Call with action=\"list\" to inspect schemas and register them for this session, or action=\"call\" with tool and arguments to run one directly.",
                    occasional_names.join(", ")
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": ["list", "call"], "default": "list"},
                        "tool": {"type": "string"},
                        "arguments": {"type": "object"}
                    }
                }
            }));
        }
        listed
    }

    fn call(&mut self, name: Option<&str>, arguments: Value) -> (Result<Value, String>, bool) {
        let tool_name = name.unwrap_or_default();
        if let Some(role) = self.role {
            if self.lazy && tool_name == LOADER_TOOL {
                return self.call_loader(role, &arguments);
            }
            if !tool_access(role, McpToolServer::Run, tool_name).is_allowed() {
                return (
                    Err(format!(
                        "Tool `velocity-run.{tool_name}` is not allowed for `{}` threads.",
                        role.as_str()
                    )),
                    false,
                );
            }
        }
        (self.call_direct(Some(tool_name), arguments), false)
    }

    fn call_loader(
        &mut self,
        role: ThreadRole,
        arguments: &Value,
    ) -> (Result<Value, String>, bool) {
        match arguments["action"].as_str().unwrap_or("list") {
            "list" => {
                let schemas: Vec<Value> = Self::all_tools()
                    .into_iter()
                    .filter(|entry| {
                        entry["name"].as_str().is_some_and(|name| {
                            tool_access(role, McpToolServer::Run, name) == ToolAccess::Occasional
                        })
                    })
                    .collect();
                let changed = !self.occasional_loaded;
                self.occasional_loaded = true;
                (Ok(json!({"loaded": true, "tools": schemas})), changed)
            }
            "call" => {
                let inner_name = arguments["tool"].as_str().unwrap_or_default();
                if !tool_access(role, McpToolServer::Run, inner_name).is_allowed() {
                    return (
                        Err(format!(
                            "Tool `velocity-run.{inner_name}` is not allowed for `{}` threads.",
                            role.as_str()
                        )),
                        false,
                    );
                }
                let inner_args = arguments
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                (self.call_direct(Some(inner_name), inner_args), false)
            }
            other => (
                Err(format!(
                    "Unsupported action `{other}`; use `list` or `call`."
                )),
                false,
            ),
        }
    }

    fn call_direct(&mut self, name: Option<&str>, arguments: Value) -> Result<Value, String> {
        let started = Instant::now();
        let result = match name {
            Some(QUEUED_RUN_TOOL) => request_queued_run(self.port, &self.token, arguments),
            Some(RUN_STATUS_TOOL) => request_run_status(self.port, &self.token, arguments),
            Some(RUN_CANCEL_TOOL) => request_run_cancel(self.port, &self.token, arguments),
            Some(WORKFLOW_RUN_TOOL) => request_workflow_run(self.port, &self.token, arguments),
            Some(PREFLIGHT_TOOL) => request_preflight(self.port, &self.token, arguments),
            _ => Err("Unknown queued run tool".into()),
        };
        self.latency.record(
            name.unwrap_or("unknown"),
            Duration::ZERO,
            started.elapsed(),
            result.is_ok(),
        );
        result
    }
}

fn serve_stdio(server: &mut Server, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    for line in input.lines() {
        let line = line?;
        let (response, list_changed) = match serde_json::from_str::<Value>(&line) {
            Ok(request) => respond(server, &request),
            Err(_) => (
                Some(
                    json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
                ),
                false,
            ),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut output, &response)?;
            writeln!(output)?;
            if list_changed {
                writeln!(
                    output,
                    "{}",
                    json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})
                )?;
            }
            output.flush()?;
        }
    }
    Ok(())
}

fn respond(server: &mut Server, request: &Value) -> (Option<Value>, bool) {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
        return (
            Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32600,"message":"Invalid request"}}),
            ),
            false,
        );
    }
    if request.get("id").is_none() {
        return (None, false);
    }
    let mut list_changed = false;
    let result = match method.unwrap_or_default() {
        "initialize" => {
            let tools_cap = if server.role.is_some() && server.lazy {
                json!({"listChanged": true})
            } else {
                json!({})
            };
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": tools_cap},
                "serverInfo": {"name": QUEUED_RUN_SERVER, "version": env!("CARGO_PKG_VERSION")},
                "instructions": server.instructions(),
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": server.tools()}),
        "tools/call" => {
            let parameters = &request["params"];
            let name = parameters.get("name").and_then(Value::as_str);
            let arguments = parameters
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let (outcome, changed) = server.call(name, arguments);
            list_changed = changed;
            let (text, failed) = match outcome {
                Ok(value) => (value.to_string(), false),
                Err(error) => (error, true),
            };
            let text = tool_answer(
                text,
                "Use one path or issue number, a narrower query, or a smaller page when the tool supports it.",
            );
            json!({"isError": failed, "content": [{"type": "text", "text": text}]})
        }
        _ => {
            return (
                Some(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
                ),
                false,
            );
        }
    };
    (
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
        list_changed,
    )
}
