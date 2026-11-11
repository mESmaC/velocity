//! Starts a thread's AIRC MCP server exactly as `session/new` passes it to the agent (F14),
//! then speaks MCP to it the way an agent does: `initialize` (joins the channel),
//! `tools/list` and a `list_nicks` call.
//!
//! `cargo run -p velocity-acp --example airc_server -- <velocity executable> <app-data> <worktree>`

// A console example: its children share its console, so no hidden_command (#232).
#![allow(clippy::disallowed_methods)]

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use agent_client_protocol::v1::schema::McpServer;
use velocity_acp::client::AircLaunch;

const REQUESTS: [&str; 3] = [
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"example","version":"0"}}}"#,
    r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_nicks","arguments":{}}}"#,
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1).map(PathBuf::from);
    let executable = arguments.next().ok_or("missing <velocity executable>")?;
    let app_data = arguments.next().ok_or("missing <app-data>")?;
    let worktree = arguments.next().ok_or("missing <worktree>")?;
    let server = AircLaunch::new(executable, app_data).thread(1, 7, &worktree);
    let McpServer::Stdio(server) = server else {
        return Err("the AIRC server is not a stdio server".into());
    };
    println!("server   {}", server.name);
    println!(
        "command  {} {}",
        server.command.display(),
        server.args.join(" ")
    );
    let mut child = Command::new(&server.command)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut input = child.stdin.take().ok_or("no stdin")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("no stdout")?);
    for request in REQUESTS {
        writeln!(input, "{request}")?;
        input.flush()?;
        let mut line = String::new();
        output.read_line(&mut line)?;
        println!("reply    {}", line.trim_end());
    }
    // End of input is how an agent leaves the channel.
    drop(input);
    println!("exited   {}", child.wait()?);
    Ok(())
}
