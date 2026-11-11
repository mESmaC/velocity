//! Adds a shared MCP server through Settings → Agents' edit, launches a provider's agent the
//! way every role does (`agent_launch` → `spawn_session`), and asks it to call the server's
//! `shout` tool (SPEC.md §3.2 Tool access). Prints the agent's tool calls and reply.
//!
//! `cargo run -p velocity-threads --example shared_mcp -- <state-dir> <worktree> <claude|codex|kimi|antigravity> <name> <command or URL>`

use std::path::PathBuf;

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate, TextContent};
use tokio::sync::mpsc::unbounded_channel;
use velocity_acp::client::{AgentEvent, AgentInput, SessionSetup};
use velocity_store::shared_mcp::SharedMcpServerEdit;
use velocity_store::{ProviderKind, StateStore};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let mut next = |what: &str| arguments.next().ok_or(format!("missing <{what}>"));
    let state = PathBuf::from(next("state-dir")?);
    let worktree = PathBuf::from(next("worktree")?);
    let provider = match next("provider")?.as_str() {
        "claude" => ProviderKind::Claude,
        "codex" => ProviderKind::Codex,
        "kimi" => ProviderKind::Kimi,
        "antigravity" => ProviderKind::Antigravity,
        other => return Err(format!("unknown provider {other}").into()),
    };
    let name = next("name")?;
    let target = next("command or URL")?;
    let mut store = StateStore::load_or_default(state)?;
    let saved = store.edit_shared_mcp_servers(SharedMcpServerEdit::Add {
        name: name.clone(),
        target,
    });
    println!("saved    {saved:?}");
    let settings = store.settings();
    let (launch, bypass_modes) = velocity_threads::agent_launch(&settings, provider)?;
    println!("passing  {:?}", launch.shared_servers);
    let (inputs, receiver) = unbounded_channel();
    let prompt = format!(
        "Call the `shout` tool of the `{name}` MCP server once, then reply with exactly what \
         it returned. Do nothing else."
    );
    inputs.send(AgentInput::Send(vec![ContentBlock::Text(
        TextContent::new(prompt),
    )]))?;
    let setup = SessionSetup::new(worktree, bypass_modes);
    let mut session = velocity_threads::spawn_session(launch, setup, receiver);
    while let Some(event) = session.events.blocking_recv() {
        if print(&event) {
            let _ = inputs.send(AgentInput::Stop);
        }
    }
    println!("outcome  {:?}", session.outcome.blocking_recv());
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
