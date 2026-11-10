//! Files explicitly identified as mutations by ACP (SPEC.md F14).
use crate::client::AgentEvent;
use agent_client_protocol::v1::schema::{SessionUpdate, ToolKind};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Default)]
struct ObservedTool {
    kind: Option<ToolKind>,
    locations: Vec<PathBuf>,
}

/// Session-local tool metadata: partial updates inherit the last kind and locations.
#[derive(Default)]
pub struct ToolMutations {
    tools: BTreeMap<String, ObservedTool>,
}

impl ToolMutations {
    pub fn observe(&mut self, event: &AgentEvent) -> Vec<PathBuf> {
        match event {
            AgentEvent::FileWritten { path } => vec![path.clone()],
            AgentEvent::Transcript { update, .. } => self.update(update),
            _ => Vec::new(),
        }
    }

    pub fn update(&mut self, update: &SessionUpdate) -> Vec<PathBuf> {
        let tool = match update {
            SessionUpdate::ToolCall(call) => {
                let tool = self
                    .tools
                    .entry(call.tool_call_id.0.to_string())
                    .or_default();
                tool.kind = Some(call.kind);
                tool.locations = call
                    .locations
                    .iter()
                    .map(|location| location.path.clone())
                    .collect();
                tool
            }
            SessionUpdate::ToolCallUpdate(update) => {
                let tool = self
                    .tools
                    .entry(update.tool_call_id.0.to_string())
                    .or_default();
                if let Some(kind) = update.fields.kind {
                    tool.kind = Some(kind);
                }
                if let Some(locations) = &update.fields.locations {
                    tool.locations = locations
                        .iter()
                        .map(|location| location.path.clone())
                        .collect();
                }
                tool
            }
            _ => return Vec::new(),
        };
        // Execute/Other and unclassified calls may merely inspect files. Agents claim
        // those edits explicitly; actual fs writes are independently reported above.
        if matches!(
            tool.kind,
            Some(ToolKind::Edit | ToolKind::Delete | ToolKind::Move)
        ) {
            tool.locations.clone()
        } else {
            Vec::new()
        }
    }
}
