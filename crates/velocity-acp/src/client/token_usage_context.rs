use std::collections::{BTreeMap, HashMap};

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate, ToolCallContent, ToolKind};
use serde_json::Value;
use velocity_graph::token_usage::{Counts, Input};

#[derive(Clone, Default)]
pub(super) struct Context(BTreeMap<Input, u64>);

impl Context {
    pub(super) fn add(&mut self, source: Input, bytes: usize) {
        let estimate = (bytes as u64).div_ceil(4);
        let tokens = self.0.entry(source).or_default();
        *tokens = tokens.saturating_add(estimate);
    }

    pub(super) fn replace(&mut self, source: Input, bytes: usize) {
        self.0.remove(&source);
        self.add(source, bytes);
    }

    pub(super) fn clear_history(&mut self) {
        self.0
            .retain(|source, _| matches!(source, Input::BasePrompt | Input::ProjectInstructions));
    }

    pub(super) fn attribute(&self, mut counts: Counts) -> Counts {
        let estimated: u64 = self.0.values().copied().sum();
        let denominator = estimated.max(counts.input).max(1);
        let mut remaining = counts.input;
        for (&source, &tokens) in &self.0 {
            let scaled =
                ((u128::from(tokens) * u128::from(counts.input)) / u128::from(denominator)) as u64;
            if scaled > 0 {
                counts.sources.insert(source, scaled);
                remaining -= scaled;
            }
        }
        if remaining > 0 {
            counts.sources.insert(Input::Unattributed, remaining);
        }
        counts
    }
}

pub(super) fn value_size(value: &Value) -> usize {
    match value {
        Value::Null => 0,
        Value::String(text) => text.len(),
        Value::Array(items) => items.iter().map(value_size).sum(),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| key.len() + value_size(value))
            .sum(),
        _ => 4,
    }
}

pub(super) fn message_source(text: &str) -> Input {
    if text.contains("# AGENTS.md instructions for")
        || text.starts_with("Project instructions from AGENTS.md:")
        || text.starts_with("<system-reminder>\nContents of ")
    {
        Input::ProjectInstructions
    } else {
        Input::Conversation
    }
}

pub(super) fn tool_source(name: &str, kind: Option<ToolKind>, input: Option<&Value>) -> Input {
    let name = name.to_ascii_lowercase();
    let command = input
        .and_then(|value| {
            value
                .as_str()
                .or_else(|| value["command"].as_str())
                .or_else(|| value["cmd"].as_str())
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    if [
        "graph_find",
        "graph_read",
        "graph_spec",
        "graph_impact",
        "graph_run",
        "graph_overlaps",
        "graph_grep",
        "graph_glob",
    ]
    .iter()
    .any(|tool| name.ends_with(tool))
    {
        return Input::GraphCalls;
    }
    if (name.ends_with("write_stdin")
        && input.is_none_or(|value| value["chars"].as_str().is_none_or(str::is_empty)))
        || ["airc_poll", "airc_wait", "wait_agent", "rosen__status"]
            .iter()
            .any(|tool| name.ends_with(tool))
        || command.starts_with("rosen status")
    {
        return Input::Polls;
    }
    if command.contains("git diff")
        || command.contains("rosen diff")
        || name.ends_with("rosen__diff")
    {
        return Input::Diffs;
    }
    if [
        "cargo ",
        "pnpm ",
        "npm ",
        "lake ",
        "scripts/check",
        "rosen run",
    ]
    .iter()
    .any(|word| command.contains(word))
    {
        return Input::BuildOutput;
    }
    let words =
        || command.split(|character: char| !character.is_ascii_alphanumeric() && character != '-');

    if matches!(kind, Some(ToolKind::Search))
        || matches!(name.as_str(), "glob" | "grep" | "search" | "websearch")
        || ["rg", "grep", "findstr"]
            .iter()
            .any(|word| words().any(|token| token == *word))
    {
        return Input::Searches;
    }
    if matches!(kind, Some(ToolKind::Read))
        || matches!(name.as_str(), "read" | "read_file" | "read_text_file")
        || ["cat", "sed", "get-content", "head", "tail"]
            .iter()
            .any(|word| words().any(|token| token == *word))
    {
        return Input::CodeReads;
    }
    if name.starts_with("mcp") {
        return Input::McpTools;
    }
    Input::Unattributed
}

#[derive(Default)]
pub(super) struct AcpContext {
    pub context: Context,
    tools: HashMap<String, (Input, usize)>,
}

impl AcpContext {
    pub(super) fn observe(&mut self, update: &SessionUpdate) {
        match update {
            SessionUpdate::ToolCall(call) => self.tool(
                &call.tool_call_id.0,
                Some(call.name.as_deref().unwrap_or(&call.title)),
                Some(call.kind),
                call.raw_input.as_ref(),
                call.raw_output.as_ref(),
                Some(&call.content),
            ),
            SessionUpdate::ToolCallUpdate(call) => self.tool(
                &call.tool_call_id.0,
                call.fields.name.as_deref().or(call.fields.title.as_deref()),
                call.fields.kind,
                call.fields.raw_input.as_ref(),
                call.fields.raw_output.as_ref(),
                call.fields.content.as_deref(),
            ),
            SessionUpdate::AgentMessageChunk(chunk) | SessionUpdate::AgentThoughtChunk(chunk) => {
                if let ContentBlock::Text(text) = &chunk.content {
                    self.context.add(Input::Conversation, text.text.len());
                }
            }
            _ => {}
        }
    }

    fn tool(
        &mut self,
        id: &str,
        name: Option<&str>,
        kind: Option<ToolKind>,
        input: Option<&Value>,
        output: Option<&Value>,
        content: Option<&[ToolCallContent]>,
    ) {
        let (source, size) = self
            .tools
            .entry(id.to_owned())
            .or_insert((Input::Unattributed, 0));
        let classified = tool_source(name.unwrap_or_default(), kind, input);
        if classified != Input::Unattributed {
            *source = classified;
        }
        if let Some(bytes) = output.map(value_size).or_else(|| {
            content.map(|blocks| {
                blocks
                    .iter()
                    .map(|block| match block {
                        ToolCallContent::Content(block) => match &block.content {
                            ContentBlock::Text(text) => text.text.len(),
                            _ => 0,
                        },
                        ToolCallContent::Diff(diff) => {
                            diff.new_text.len() + diff.old_text.as_ref().map_or(0, String::len)
                        }
                        _ => 0,
                    })
                    .sum()
            })
        }) {
            *size = (*size).max(bytes);
        }
    }

    pub(super) fn snapshot(&self) -> Context {
        let mut result = self.context.clone();
        for &(source, bytes) in self.tools.values() {
            result.add(source, bytes);
        }
        result
    }
}
