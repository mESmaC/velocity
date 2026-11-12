//! Runs one real agent turn with the servers a thread gets (AIRC and the code graph, F14, F19)
//! and asks the agent to answer a question about its worktree's code with the graph tools,
//! printing its tool calls and reply.
//!
//! `cargo run -p velocity-acp --example graph_agent -- <velocity executable> <app-data> <worktree> <agent> [args...]`

use std::path::PathBuf;

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate, TextContent};
use tokio::sync::mpsc::unbounded_channel;
use velocity_acp::client::{AgentEvent, AgentInput, AgentLaunch, AircLaunch, SessionSetup};

const PROMPT: &str = "Using the velocity-graph MCP tools, find every function or method in \
this repository that calls `piece_key`, and the file and line of each call. Reply with one line \
per caller. Do not edit anything.";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let executable = PathBuf::from(arguments.next().ok_or("missing <velocity executable>")?);
    let app_data = PathBuf::from(arguments.next().ok_or("missing <app-data>")?);
    let worktree = PathBuf::from(arguments.next().ok_or("missing <worktree>")?);
    let mut agent = AgentLaunch::new(arguments.next().ok_or("missing <agent>")?.into());
    agent.args = arguments.collect();
    let mut setup = SessionSetup::new(worktree.clone(), Vec::new());
    setup.airc = Some(AircLaunch::new(executable, app_data).thread(1, 7, &worktree));
    let (inputs, receiver) = unbounded_channel();
    let (events, mut received) = unbounded_channel();
    let prompt = ContentBlock::Text(TextContent::new(PROMPT));
    inputs.send(AgentInput::Send(vec![prompt]))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let session = runtime.spawn(velocity_acp::client::run_session(
        agent, setup, receiver, events,
    ));
    runtime.block_on(async {
        while let Some(event) = received.recv().await {
            if print(&event) {
                let _ = inputs.send(AgentInput::Stop);
            }
        }
    });
    runtime.block_on(session)??;
    Ok(())
}

/// Prints what matters; `true` once the turn ended.
fn print(event: &AgentEvent) -> bool {
    match event {
        AgentEvent::Transcript { update, .. } | AgentEvent::SessionUpdate(update) => {
            match update {
                SessionUpdate::ToolCall(call) => println!("tool     {}", call.title),
                SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(text) = &chunk.content {
                        print!("{}", text.text);
                    }
                }
                _ => {}
            }
            false
        }
        AgentEvent::TurnEnded(outcome) => {
            println!("\nturn     {outcome:?}");
            true
        }
        _ => false,
    }
}
