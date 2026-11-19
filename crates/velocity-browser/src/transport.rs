//! ACP launches a small stdio server; a loopback route keeps browser ownership in Velocity.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::{
    BrowserHub, Result, error,
    tools::{self, Output},
};

pub const SERVER: &str = "velocity-browser";
pub const SWITCH: &str = "--browser-mcp";
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
    let port = std::env::var(PORT_ENV)
        .ok()
        .and_then(|port| port.parse::<u16>().ok())
        .ok_or_else(|| error("Missing browser tool port"))?;
    let token = std::env::var(TOKEN_ENV)
        .ok()
        .and_then(|token| Uuid::parse_str(&token).ok())
        .ok_or_else(|| error("Missing browser tool session"))?;
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
        if let Some(response) = reply(port, token, request) {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
}

fn reply(port: u16, token: Uuid, request: Value) -> Option<Value> {
    let id = request.get("id")?;
    let result = match request["method"].as_str()? {
        "initialize" => json!({
            "protocolVersion": request["params"]["protocolVersion"].as_str().unwrap_or("2024-11-05"),
            "capabilities":{"tools":{}},"serverInfo":{"name":SERVER,"version":"0.1.0"},
            "instructions":"Use snapshot to read pages and screenshot only for visual checks. Ask the owner to take over for sign-in. Never type passwords, tokens or API keys.",
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools":tools::definitions()}),
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or_default().to_owned();
            let arguments = params["arguments"].clone();
            match forward(
                port,
                Request {
                    token,
                    name,
                    arguments,
                },
            ) {
                Ok(value) => value,
                Err(error) => contents(Err(error)),
            }
        }
        _ => {
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            );
        }
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}

fn forward(port: u16, request: Request) -> Result<Value> {
    let address = (Ipv4Addr::LOCALHOST, port).into();
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
    socket.set_read_timeout(Some(CALL_TIMEOUT + Duration::from_secs(3)))?;
    socket.set_write_timeout(Some(Duration::from_secs(2)))?;
    writeln!(socket, "{}", serde_json::to_string(&request)?)?;
    let mut line = Vec::new();
    BufReader::new(socket)
        .take(RESPONSE_BYTES)
        .read_until(b'\n', &mut line)?;
    Ok(serde_json::from_slice(&line)?)
}
