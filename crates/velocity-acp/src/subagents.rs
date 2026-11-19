//! Subagent activity inside a parent session (SPEC.md §3.2 Subagents, #436).
//!
//! A subagent is keyed by the parent's tool call that spawned it. claude-agent-acp stamps
//! every update made inside an `Agent` or `Task` call with that call's id in
//! `_meta.claudeCode.parentToolUseId`, and reports the call's `tool_response` (tokens,
//! background launch) in `_meta.claudeCode.toolResponse`. codex-acp reports a `spawnAgent`
//! collab call whose raw input names the model, the prompt and the child threads, and whose
//! `agentsStates` (on every collab call) say how each child is doing.
//!
//! An agent told that Velocity takes native subagent sessions (`clientCapabilities.subagents`,
//! which codex-acp needs before it forwards a child thread's items) instead gives each
//! subagent an ACP session of its own. The parent session announces it with a
//! `subagent_update` and reports its state there; the subagent's messages and tool calls
//! arrive as `session/update`s under its own session id, which is then its key.
//!
//! Everything here is pure: the router files updates, the report functions read facts.

use std::collections::HashMap;
use std::sync::Arc;

use agent_client_protocol::v1::schema::{
    IdleStopReason, Meta, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionUpdate, StateUpdate, ToolCallStatus,
};
use serde_json::Value;

use crate::{EntryClass, Placement, Transcript};

/// Claude Code's tools that start a subagent (`Task` before Claude Code 2, `Agent` after).
const CLAUDE_SUBAGENT_TOOLS: [&str; 2] = ["Agent", "Task"];
/// The codex-acp collab call that starts subagents.
const CODEX_SPAWN_TOOL: &str = "spawnAgent";
/// Longest description taken from a prompt when the agent gives no short one.
const DESCRIPTION_CHARACTERS: usize = 80;

/// The spawning tool call an update's own `_meta` attributes it to.
pub fn attributed_spawn(update: &SessionUpdate) -> Option<&str> {
    claude_meta(update_meta(update)?)?
        .get("parentToolUseId")?
        .as_str()
        .filter(|identifier| !identifier.is_empty())
}

/// Files each subagent's updates in that subagent's own transcript, so its steps never
/// appear as the parent's.
#[derive(Default)]
pub struct SubagentRouter {
    /// The subagent each of its tool calls belongs to. A later update for the call may
    /// lack the attribution: claude-agent-acp's post-tool hook reports carry only the
    /// tool name.
    tool_calls: HashMap<Arc<str>, Arc<str>>,
    transcripts: HashMap<Arc<str>, Transcript<Arc<str>, ()>>,
    /// The native subagent sessions announced so far, by session id.
    sessions: HashMap<Arc<str>, NativeSession>,
}

/// A native subagent session its parent announced.
struct NativeSession {
    /// The announcement of a session known only from replayed history. It is held back
    /// until the session works again, so a resumed thread shows no empty subagents.
    held: Option<SessionUpdate>,
}

/// Where a `session/update` that concerns a native subagent session goes.
#[derive(Clone, Debug, PartialEq)]
pub enum SessionRoute {
    /// An entry in the transcript of the subagent `spawn`. `announcement` is the held
    /// announcement of a session from replayed history, to report first.
    Entry {
        spawn: Arc<str>,
        placement: Placement,
        announcement: Option<SessionUpdate>,
    },
    /// A fact about the subagent `spawn` for [`session_report`]: its announcement or state
    /// from its parent, or its own model or token use.
    Fact { spawn: Arc<str> },
    /// Replayed history, or a fact about a session that has not worked since the replay.
    Skipped,
}

impl SubagentRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The subagent that made `update` and where it went in that subagent's transcript, or
    /// `None` for the parent's own updates.
    pub fn route(
        &mut self,
        update: &SessionUpdate,
        class: &EntryClass<Arc<str>>,
    ) -> Option<(Arc<str>, Placement)> {
        let spawn = self.spawn_of(update)?;
        let placement = self
            .transcripts
            .entry(Arc::clone(&spawn))
            .or_default()
            .record(class.clone(), ());
        Some((spawn, placement))
    }

    /// Routes one `session/update` of `session`: a subagent's announcement, or anything a
    /// native subagent session reports. `None` leaves the update to its own session, where
    /// [`SubagentRouter::route`] looks for an attributed subagent. `replaying` marks history
    /// that `session/load` replays, which rebuilds no subagent transcript.
    pub fn route_session(
        &mut self,
        session: &Arc<str>,
        update: &SessionUpdate,
        class: Option<&EntryClass<Arc<str>>>,
        replaying: bool,
    ) -> Option<SessionRoute> {
        if let SessionUpdate::SubagentUpdate(child) = update {
            let spawn = Arc::clone(&child.session_id.0);
            let native = self
                .sessions
                .entry(Arc::clone(&spawn))
                .or_insert_with(|| NativeSession {
                    held: replaying.then(|| update.clone()),
                });
            return Some(match native.held {
                Some(_) => SessionRoute::Skipped,
                None => SessionRoute::Fact { spawn },
            });
        }
        let native = self.sessions.get_mut(session)?;
        if replaying {
            return Some(SessionRoute::Skipped);
        }
        let Some(class) = class else {
            return Some(match native.held {
                Some(_) => SessionRoute::Skipped,
                None => SessionRoute::Fact {
                    spawn: Arc::clone(session),
                },
            });
        };
        let announcement = native.held.take();
        let placement = self
            .transcripts
            .entry(Arc::clone(session))
            .or_default()
            .record(class.clone(), ());
        Some(SessionRoute::Entry {
            spawn: Arc::clone(session),
            placement,
            announcement,
        })
    }

    fn spawn_of(&mut self, update: &SessionUpdate) -> Option<Arc<str>> {
        let call = tool_call_id(update);
        if let Some(spawn) = attributed_spawn(update) {
            let spawn: Arc<str> = Arc::from(spawn);
            if let Some(call) = call {
                self.tool_calls.insert(Arc::clone(call), Arc::clone(&spawn));
            }
            return Some(spawn);
        }
        call.and_then(|call| self.tool_calls.get(call).cloned())
    }
}

/// A subagent's foreground work, as its native session's state reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentActivity {
    /// Working, or blocked on an answer it asked for.
    Running,
    /// Its turn ended normally. Its parent can still message it again.
    Done,
    /// Its turn ended on an error or was cancelled.
    Failed,
    /// The agent cannot tell: the session was closed, unloaded or lost.
    Unknown,
}

/// What a parent's tool call, or a native subagent session, says about a subagent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubagentReport {
    /// The subagent's key: the spawning tool call's id, or its native session's id.
    pub spawn: String,
    /// The tool call's title.
    pub title: Option<String>,
    /// The short task description the parent gave, or the start of its prompt.
    pub description: Option<String>,
    /// The model the parent asked for; `None` inherits the parent's.
    pub model: Option<String>,
    /// The spawning call's status.
    pub status: Option<ToolCallStatus>,
    /// The native session's state.
    pub activity: Option<SubagentActivity>,
    /// Total tokens the subagent used, when the agent reports them.
    pub tokens: Option<u64>,
    /// The call returned while the subagent keeps working: a Claude background subagent, or
    /// any Codex spawn, which returns once the child thread exists.
    pub background: bool,
    /// Codex child threads this call started.
    pub threads: Vec<String>,
}

/// The subagent facts in a parent update: a call that spawns a subagent, or an update to a
/// call `is_spawn` already knows as one.
pub fn subagent_report(
    update: &SessionUpdate,
    is_spawn: impl Fn(&str) -> bool,
) -> Option<SubagentReport> {
    match update {
        SessionUpdate::ToolCall(call)
            if spawns_subagent(
                Some(&call.title),
                call.meta.as_ref(),
                call.raw_input.as_ref(),
            ) =>
        {
            Some(report(
                &call.tool_call_id.0,
                Some(&call.title),
                call.raw_input.as_ref(),
                call.raw_output.as_ref(),
                call.meta.as_ref(),
                Some(call.status),
            ))
        }
        SessionUpdate::ToolCallUpdate(update) => {
            let fields = &update.fields;
            let spawning = is_spawn(&update.tool_call_id.0)
                || spawns_subagent(
                    fields.title.as_deref(),
                    update.meta.as_ref(),
                    fields.raw_input.as_ref(),
                );
            spawning.then(|| {
                report(
                    &update.tool_call_id.0,
                    fields.title.as_deref(),
                    fields.raw_input.as_ref(),
                    fields.raw_output.as_ref(),
                    update.meta.as_ref(),
                    fields.status,
                )
            })
        }
        _ => None,
    }
}

/// The subagent facts in an update [`SessionRoute::Fact`] filed under `spawn`: the parent's
/// `subagent_update` (title, task and state), or the subagent session's own model option
/// and context use.
pub fn session_report(spawn: &str, update: &SessionUpdate) -> Option<SubagentReport> {
    let report = SubagentReport {
        spawn: spawn.to_owned(),
        // A native session outlives the call that started it, as a Codex spawn does.
        background: true,
        ..SubagentReport::default()
    };
    match update {
        SessionUpdate::SubagentUpdate(child) => Some(SubagentReport {
            title: announced_text(child.title.value()).map(str::to_owned),
            // codex-acp describes a subagent by the whole prompt it was started with.
            description: announced_text(child.description.value()).map(prompt_summary),
            activity: child.state.value().map(state_activity),
            ..report
        }),
        SessionUpdate::ConfigOptionUpdate(options) => {
            let model = options.config_options.iter().find_map(model_choice)?;
            Some(SubagentReport {
                model: Some(model),
                ..report
            })
        }
        // The same measure as Claude's `totalTokens`: the tokens of its latest request.
        SessionUpdate::UsageUpdate(usage) => Some(SubagentReport {
            tokens: Some(usage.used),
            ..report
        }),
        _ => None,
    }
}

fn announced_text(text: Option<&String>) -> Option<&str> {
    text.map(|text| text.trim()).filter(|text| !text.is_empty())
}

/// A turn that ends on an error or a cancel failed; any other end is a finished turn.
fn state_activity(state: &StateUpdate) -> SubagentActivity {
    match state {
        StateUpdate::Running(_) | StateUpdate::RequiresAction(_) => SubagentActivity::Running,
        StateUpdate::Idle(idle) => match idle.stop_reason {
            Some(IdleStopReason::Error(_) | IdleStopReason::Cancelled) => SubagentActivity::Failed,
            _ => SubagentActivity::Done,
        },
        _ => SubagentActivity::Unknown,
    }
}

/// The model a session's config options say it runs on.
fn model_choice(option: &SessionConfigOption) -> Option<String> {
    match &option.kind {
        SessionConfigKind::Select(select)
            if option.category == Some(SessionConfigOptionCategory::Model) =>
        {
            Some(select.current_value.0.to_string())
        }
        _ => None,
    }
}

/// How each Codex child thread is doing, from a collab call's `agentsStates`: running
/// threads are in progress, finished ones completed, and errored, shut down, missing or
/// interrupted ones failed.
pub fn agent_thread_states(update: &SessionUpdate) -> Vec<(String, ToolCallStatus)> {
    let raw_input = match update {
        SessionUpdate::ToolCall(call) => call.raw_input.as_ref(),
        SessionUpdate::ToolCallUpdate(update) => update.fields.raw_input.as_ref(),
        _ => None,
    };
    let Some(states) = raw_input
        .and_then(|input| input.get("agentsStates"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    states
        .iter()
        .filter_map(|(thread, state)| {
            let status = match state.get("status")?.as_str()? {
                "pendingInit" | "running" => ToolCallStatus::InProgress,
                "completed" => ToolCallStatus::Completed,
                _ => ToolCallStatus::Failed,
            };
            Some((thread.clone(), status))
        })
        .collect()
}

fn report(
    spawn: &str,
    title: Option<&str>,
    raw_input: Option<&Value>,
    raw_output: Option<&Value>,
    meta: Option<&Meta>,
    status: Option<ToolCallStatus>,
) -> SubagentReport {
    let input_text = |key: &str| {
        raw_input
            .and_then(|input| input.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let response = meta
        .and_then(claude_meta)
        .and_then(|claude| claude.get("toolResponse"))
        .or(raw_output);
    let codex = codex_spawn(title, raw_input);
    SubagentReport {
        spawn: spawn.to_owned(),
        title: title.map(str::to_owned),
        description: input_text("description")
            .map(str::to_owned)
            .or_else(|| input_text("prompt").map(prompt_summary)),
        model: input_text("model").map(str::to_owned),
        status,
        activity: None,
        tokens: response
            .and_then(|response| response.get("totalTokens"))
            .and_then(Value::as_u64),
        background: codex || launched_in_background(raw_input, response),
        threads: raw_input
            .and_then(|input| input.get("receiverThreadIds"))
            .and_then(Value::as_array)
            .map(|threads| {
                threads
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// A Claude subagent started with `run_in_background`, or whose call came back
/// `async_launched`, keeps working after its call completes.
fn launched_in_background(raw_input: Option<&Value>, response: Option<&Value>) -> bool {
    let requested = raw_input
        .and_then(|input| input.get("run_in_background"))
        .and_then(Value::as_bool)
        == Some(true);
    let launched = response.is_some_and(|response| {
        response.get("isAsync").and_then(Value::as_bool) == Some(true)
            || response.get("status").and_then(Value::as_str) == Some("async_launched")
    });
    requested || launched
}

fn spawns_subagent(title: Option<&str>, meta: Option<&Meta>, raw_input: Option<&Value>) -> bool {
    let claude_tool = meta
        .and_then(claude_meta)
        .and_then(|claude| claude.get("toolName"))
        .and_then(Value::as_str);
    claude_tool.is_some_and(|tool| CLAUDE_SUBAGENT_TOOLS.contains(&tool))
        || codex_spawn(title, raw_input)
}

fn codex_spawn(title: Option<&str>, raw_input: Option<&Value>) -> bool {
    title == Some(CODEX_SPAWN_TOOL)
        && raw_input.is_some_and(|input| input.get("receiverThreadIds").is_some())
}

/// The first line of a prompt, cut to a card's width.
fn prompt_summary(prompt: &str) -> String {
    let line = prompt.lines().next().unwrap_or_default().trim();
    if line.chars().count() <= DESCRIPTION_CHARACTERS {
        return line.to_owned();
    }
    let mut summary: String = line.chars().take(DESCRIPTION_CHARACTERS - 1).collect();
    summary.push('…');
    summary
}

fn update_meta(update: &SessionUpdate) -> Option<&Meta> {
    match update {
        SessionUpdate::UserMessageChunk(chunk)
        | SessionUpdate::AgentMessageChunk(chunk)
        | SessionUpdate::AgentThoughtChunk(chunk) => chunk.meta.as_ref(),
        SessionUpdate::ToolCall(call) => call.meta.as_ref(),
        SessionUpdate::ToolCallUpdate(update) => update.meta.as_ref(),
        SessionUpdate::Plan(plan) => plan.meta.as_ref(),
        _ => None,
    }
}

fn claude_meta(meta: &Meta) -> Option<&Value> {
    meta.get("claudeCode")
}

fn tool_call_id(update: &SessionUpdate) -> Option<&Arc<str>> {
    match update {
        SessionUpdate::ToolCall(call) => Some(&call.tool_call_id.0),
        SessionUpdate::ToolCallUpdate(update) => Some(&update.tool_call_id.0),
        _ => None,
    }
}
