//! ACP launches a small stdio server; a loopback route keeps browser ownership in Velocity.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use velocity_rosen::mcp_latency::LatencyRecorder;

use crate::{
    BrowserHub, Result, error,
    tools::{self, Output},
};

pub const SERVER: &str = "velocity-browser";
pub const SWITCH: &str = "--browser-mcp";
/// Load the 16 browser tools on demand instead of injecting all definitions up front (#533).
pub const LAZY_FLAG: &str = "--lazy";
pub const LOADER_TOOL: &str = "browser_tools";
pub const PORT_ENV: &str = "VELOCITY_BROWSER_PORT";
pub const TOKEN_ENV: &str = "VELOCITY_BROWSER_TOKEN";
const REQUEST_BYTES: u64 = 64 * 1024;
const RESPONSE_BYTES: u64 = 2 * 1024 * 1024;
const CALL_TIMEOUT: Duration = Duration::from_secs(55);

#[derive(Deserialize, Serialize)]
struct Request {
    token: Uuid,
    name: String,
    arguments: Value,
}

pub(crate) fn listen(hub: BrowserHub) -> Result<u16> {
    let socket = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    socket.set_nonblocking(true)?;
    let port = socket.local_addr()?.port();
    let listener = tokio::net::TcpListener::from_std(socket)?;
    tokio::spawn(async move {
        let slots = Arc::new(tokio::sync::Semaphore::new(32));
        while let Ok((socket, _)) = listener.accept().await {
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let hub = hub.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let _ = serve_request(hub, socket).await;
            });
        }
    });
    Ok(port)
}

async fn serve_request(hub: BrowserHub, socket: tokio::net::TcpStream) -> Result<()> {
    let mut socket = tokio::io::BufReader::new(socket);
    let mut line = Vec::new();
    tokio::time::timeout(
        Duration::from_secs(2),
        (&mut socket)
            .take(REQUEST_BYTES)
            .read_until(b'\n', &mut line),
    )
    .await
    .map_err(|_| error("Browser request timed out"))??;
    let request: Request = serde_json::from_slice(&line)?;
    let result = tokio::time::timeout(
        CALL_TIMEOUT,
        hub.tool(request.token, &request.name, &request.arguments),
    )
    .await;
    let payload = match result {
        Ok(result) => contents(result),
        Err(_) => contents(Err(error("Browser call timed out"))),
    };
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.write_all(format!("{payload}\n").as_bytes()),
    )
    .await
    .map_err(|_| error("Browser response timed out"))??;
    Ok(())
}

fn contents(result: Result<Output>) -> Value {
    match result {
        Ok(Output::Text(text)) => json!({"content":[{"type":"text","text":text}]}),
        Ok(Output::Image { path, jpeg }) => json!({"content":[
            {"type":"text","text":path},{"type":"image","data":jpeg,"mimeType":"image/jpeg"},
        ]}),
        Err(error) => json!({"isError":true,"content":[{"type":"text","text":error.to_string()}]}),
    }
}

pub fn serve_mcp() -> Result<()> {
    let lazy = std::env::args_os()
        .skip(2)
        .any(|arg| arg.to_str() == Some(LAZY_FLAG));
    serve_mcp_with_mode(lazy)
}

pub fn serve_mcp_with_mode(lazy: bool) -> Result<()> {
    let port = std::env::var(PORT_ENV)
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .ok_or_else(|| error("Missing browser tool port"))?;
    let token = std::env::var(TOKEN_ENV)
        .ok()
        .and_then(|token| Uuid::parse_str(&token).ok())
        .ok_or_else(|| error("Missing browser tool session"))?;
    let latency = LatencyRecorder::open(SERVER, None);
    let mut loaded = !lazy;
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        if (&mut input)
            .take(REQUEST_BYTES)
            .read_until(b'\n', &mut line)?
            == 0
        {
            return Ok(());
        }
        if line.last() != Some(&b'\n') && line.len() as u64 == REQUEST_BYTES {
            return Err(error("Browser request exceeded its size limit"));
        }
        let request = serde_json::from_slice(&line)?;
        if let Some((response, list_changed)) =
            reply(port, token, request, &latency, lazy, &mut loaded)
        {
            writeln!(output, "{response}")?;
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
}

fn loader_definition() -> Value {
    json!({
        "name": LOADER_TOOL,
        "description": "Load or call the 16 agent browser tools on demand (open, navigate, back, reload, tabs, snapshot, click, type, press, select, scroll, wait_for, screenshot, console, network_errors, evaluate). Call with action=\"list\" to inspect schemas and register all browser tools for this session, or action=\"call\" with tool and arguments to run one directly.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "call"], "default": "list"},
                "tool": {"type": "string", "description": "Browser tool name (for action=\"call\")"},
                "arguments": {"type": "object", "description": "Arguments for the browser tool (for action=\"call\")"}
            }
        }
    })
}

fn reply(
    port: u16,
    token: Uuid,
    request: Value,
    latency: &LatencyRecorder,
    lazy: bool,
    loaded: &mut bool,
) -> Option<(Value, bool)> {
    let id = request.get("id")?;
    let mut list_changed = false;
    let result = match request["method"].as_str()? {
        // Shared browser guidance is published once in the session preface (src-tauri/src/browser.rs)
        // rather than repeated in MCP initialize instructions (#538).
        "initialize" => {
            latency.record("initialize", Duration::ZERO, Duration::ZERO, true);
            let tools_cap = if lazy {
                json!({"listChanged": true})
            } else {
                json!({})
            };
            json!({
                "protocolVersion": request["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
                "capabilities":{"tools": tools_cap},"serverInfo":{"name":SERVER,"version":"0.1.0"},
            })
        }
        "ping" => json!({}),
        "tools/list" => {
            let tools = if lazy && !*loaded {
                vec![loader_definition()]
            } else {
                tools::definitions()
            };
            json!({"tools": tools})
        }
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or_default().to_owned();
            let arguments = params["arguments"].clone();
            if lazy && name == LOADER_TOOL {
                let action = arguments["action"].as_str().unwrap_or("list");
                match action {
                    "list" => {
                        list_changed = !*loaded;
                        *loaded = true;
                        let text =
                            json!({"loaded": true, "tools": tools::definitions()}).to_string();
                        json!({"content":[{"type":"text","text":text}]})
                    }
                    "call" => {
                        let inner_name = arguments["tool"].as_str().unwrap_or_default().to_owned();
                        let inner_args = arguments
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| json!({}));
                        forward_call(port, token, inner_name, inner_args, latency)
                    }
                    other => contents(Err(error(format!(
                        "Unsupported action `{other}`; use `list` or `call`"
                    )))),
                }
            } else {
                forward_call(port, token, name, arguments, latency)
            }
        }
        _ => {
            return Some((
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
                false,
            ));
        }
    };
    Some((
        json!({"jsonrpc":"2.0","id":id,"result":result}),
        list_changed,
    ))
}

fn forward_call(
    port: u16,
    token: Uuid,
    name: String,
    arguments: Value,
    latency: &LatencyRecorder,
) -> Value {
    let started = Instant::now();
    match forward(
        port,
        Request {
            token,
            name: name.clone(),
            arguments,
        },
    ) {
        Ok((value, connect)) => {
            let ok = !value["isError"].as_bool().unwrap_or(false);
            latency.record(&name, connect, started.elapsed(), ok);
            value
        }
        Err(error) => {
            latency.record(&name, Duration::ZERO, started.elapsed(), false);
            contents(Err(error))
        }
    }
}

fn forward(port: u16, request: Request) -> Result<(Value, Duration)> {
    let address = (Ipv4Addr::LOCALHOST, port).into();
    let connect_started = Instant::now();
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    let connect_elapsed = connect_started.elapsed();
    socket.set_read_timeout(Some(CALL_TIMEOUT + Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    writeln!(socket, "{}", serde_json::to_string(&request)?)?;
    let mut line = Vec::new();
    BufReader::new(socket)
        .take(RESPONSE_BYTES)
        .read_until(b'\n', &mut line)?;
    Ok((serde_json::from_slice(&line)?, connect_elapsed))
}
