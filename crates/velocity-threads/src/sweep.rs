//! Duty scout MCP switch and card progress text (SPEC.md F7 Scouts).

use agent_client_protocol::v1::schema::SessionUpdate;
use velocity_acp::client::AgentEvent;
use velocity_core::scout_sweep::step_doing;

/// The command-line switch that makes Velocity's own executable serve the scout MCP server,
/// so the server ships inside the app and needs no separate install.
pub const SCOUT_SERVER_SWITCH: &str = "--scout-mcp";

/// What a scout's card says after this agent event, if it changes it: the step it is on.
pub fn sweep_doing(event: &AgentEvent) -> Option<String> {
    let update = match event {
        AgentEvent::Transcript { update, .. } | AgentEvent::SessionUpdate(update) => update,
        AgentEvent::ProviderQueued(true, provider) => {
            return Some(format!("queued \u{b7} {provider}"));
        }
        AgentEvent::ProviderQueued(false, _) => return Some("starting the agent".into()),
        _ => return None,
    };
    match update {
        SessionUpdate::ToolCall(call) => Some(step_doing(&call.title)),
        SessionUpdate::ToolCallUpdate(update) => update.fields.title.as_deref().map(step_doing),
        SessionUpdate::AgentThoughtChunk(_) => Some("thinking".into()),
        _ => None,
    }
}
