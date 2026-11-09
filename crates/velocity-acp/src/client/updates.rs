//! `session/update` notifications → the thread's transcript (§3.2, F4).

use std::sync::Arc;

use agent_client_protocol::v1::schema::{ContentChunk, SessionUpdate};

use crate::{EntryClass, MessageKey};

/// An agent-advertised slash command from `available_commands_update` (SPEC.md §3.2, F4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSlashCommand {
    pub name: String,
    pub description: String,
}

/// Extracts the agent's advertised slash commands when `update` is `available_commands_update`.
pub fn available_commands(update: &SessionUpdate) -> Option<Vec<AgentSlashCommand>> {
    match update {
        SessionUpdate::AvailableCommandsUpdate(commands) => Some(
            commands
                .available_commands
                .iter()
                .map(|command| AgentSlashCommand {
                    name: command.name.clone(),
                    description: command.description.clone(),
                })
                .collect(),
        ),
        _ => None,
    }
}

/// The transcript entry an update belongs to, or `None` for updates that are session state
/// rather than transcript: available commands (the slash menu), mode, config options, usage,
/// session info and notices.
pub fn entry_class(update: &SessionUpdate) -> Option<EntryClass<Arc<str>>> {
    match update {
        SessionUpdate::UserMessageChunk(chunk) => Some(EntryClass::UserMessage(message_key(chunk))),
        SessionUpdate::AgentMessageChunk(chunk) => {
            Some(EntryClass::AgentMessage(message_key(chunk)))
        }
        SessionUpdate::AgentThoughtChunk(chunk) => {
            Some(EntryClass::AgentThought(message_key(chunk)))
        }
        SessionUpdate::ToolCall(call) => {
            Some(EntryClass::ToolCall(Arc::clone(&call.tool_call_id.0)))
        }
        SessionUpdate::ToolCallUpdate(update) => {
            Some(EntryClass::ToolCall(Arc::clone(&update.tool_call_id.0)))
        }
        SessionUpdate::Plan(_) => Some(EntryClass::Plan),
        _ => None,
    }
}

fn message_key(chunk: &ContentChunk) -> MessageKey<Arc<str>> {
    match &chunk.message_id {
        Some(id) => MessageKey::Named(Arc::clone(&id.0)),
        None => MessageKey::Unnamed,
    }
}
