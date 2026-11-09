//! `session/update` notifications → the thread's transcript (§3.2, F4).

use std::sync::Arc;

use agent_client_protocol::v1::schema::{ContentChunk, SessionUpdate};

use crate::{EntryClass, MessageKey};

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
