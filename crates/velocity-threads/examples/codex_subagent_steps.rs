//! A Codex subagent's own steps (§3.2 Subagents): codex-acp gives each child thread an ACP
//! session of its own once the client declares `clientCapabilities.subagents`.
//!
//! Without arguments this replays what codex-acp 2.6 sends for a turn with one subagent
//! through the ACP client's routing and a thread runtime, without starting a provider
//! process. The parent session announces the child with `subagent_update` and reports its
//! state there; the child's tool calls, messages, model and context use arrive under the
//! child's session id. A `session/load` replay of the same child rebuilds nothing.
//!
//! With `--live <folder> "<prompt>"` it runs your signed-in Codex in `<folder>` instead and
//! prints the subagents the turn produced:
//!
//! `cargo run -p velocity-threads --example codex_subagent_steps -- --live <folder> "<prompt>"`
use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::v1::schema::SessionNotification;
use serde_json::{Value, json};
use velocity_acp::client::{AgentEvent, SessionSetup, entry_class};
use velocity_acp::{Provider, SessionRoute, SubagentRouter, Transcript, provider_agent};
use velocity_core::thread_view_model::{SubagentStatus, SubagentView, TranscriptBlock};
use velocity_threads::{ThreadRuntimes, spawn_session};

const ROOT: &str = "019a-root";
const CHILD: &str = "019a-root:subagent:019a-child";

type Failure = Box<dyn std::error::Error>;

fn main() -> Result<(), Failure> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.as_slice() {
        [] => replay(),
        [flag, folder, prompt] if flag == "--live" => live(PathBuf::from(folder), prompt),
        _ => Err("usage: codex_subagent_steps [--live <folder> \"<prompt>\"]".into()),
    }
}

/// The same routing `Shared::record_notification` does in the ACP client.
struct Client {
    router: SubagentRouter,
    parent: Transcript<Arc<str>, ()>,
}

impl Client {
    fn events(&mut self, notification: Value, replaying: bool) -> Result<Vec<AgentEvent>, Failure> {
        let notification: SessionNotification = serde_json::from_value(notification)?;
        let update = notification.update;
        let class = entry_class(&update);
        let route = self.router.route_session(
            &notification.session_id.0,
            &update,
            class.as_ref(),
            replaying,
        );
        Ok(match (route, class) {
            (Some(SessionRoute::Skipped), _) => Vec::new(),
            (Some(SessionRoute::Fact { spawn }), _) => {
                Vec::from([AgentEvent::SubagentSession { spawn, update }])
            }
            (
                Some(SessionRoute::Entry {
                    spawn,
                    placement,
                    announcement,
                }),
                _,
            ) => announcement
                .map(|announcement| AgentEvent::SubagentSession {
                    spawn: Arc::clone(&spawn),
                    update: *announcement,
                })
                .into_iter()
                .chain([AgentEvent::Subagent {
                    spawn,
                    placement,
                    update,
                }])
                .collect(),
            (None, Some(_)) if replaying => Vec::new(),
            (None, Some(class)) => Vec::from([AgentEvent::Transcript {
                placement: self.parent.record(class, ()),
                update,
            }]),
            (None, None) => Vec::from([AgentEvent::SessionUpdate(update)]),
        })
    }
}

fn replay() -> Result<(), Failure> {
    let key = (1, 1);
    let mut runtimes = ThreadRuntimes::default();
    let _inputs = runtimes.start(key, "W:\\work\\thread", "Investigate");
    runtimes.worktree_ready(key);
    let mut client = Client {
        router: SubagentRouter::new(),
        parent: Transcript::new(),
    };
    for notification in codex_turn() {
        for event in client.events(notification, false)? {
            runtimes.apply(key, &event);
        }
    }
    let view = runtimes.view(key).ok_or("Missing thread view")?;
    let subagent = match view.subagents.as_slice() {
        [subagent] => subagent,
        other => return Err(format!("Expected one subagent, found {}", other.len()).into()),
    };
    print_subagent(subagent);
    check_subagent(subagent)?;
    let parent = step_verbs(&view.blocks);
    println!("parent steps: {}", parent.join(", "));
    if parent.len() != 1 {
        return Err(format!("The parent shows the subagent's steps: {parent:?}").into());
    }
    check_replayed_history()
}

/// The child shows its own steps, the model and context use its session reported, and the
/// outcome its parent session reported.
fn check_subagent(subagent: &SubagentView) -> Result<(), Failure> {
    let steps = step_verbs(&subagent.blocks);
    let messages = subagent
        .blocks
        .iter()
        .filter(|block| matches!(block, TranscriptBlock::AgentMessage { .. }))
        .count();
    if subagent.identifier != CHILD
        || subagent.title != "Trace thread events"
        || subagent.model.as_deref() != Some("gpt-5.2-codex")
        || subagent.status != SubagentStatus::Done
        || subagent.tokens != Some(18_250)
        || steps.len() != 2
        || messages != 1
    {
        return Err(format!("The subagent is wrong: {subagent:?}").into());
    }
    Ok(())
}

/// `session/load` replays the child too. Nothing is rebuilt from it, and a state snapshot
/// afterwards shows no empty subagent; the child reappears, named, once it works again.
fn check_replayed_history() -> Result<(), Failure> {
    let mut client = Client {
        router: SubagentRouter::new(),
        parent: Transcript::new(),
    };
    let mut replayed = 0;
    for notification in codex_turn() {
        replayed += client.events(notification, true)?.len();
    }
    let snapshot = client.events(state(json!({"state": "idle"})), false)?;
    let resumed = client.events(child_tool("c9", "execute", "cargo fmt"), false)?;
    let named = matches!(resumed.first(), Some(AgentEvent::SubagentSession { .. }));
    if replayed != 0 || !snapshot.is_empty() || resumed.len() != 2 || !named {
        return Err(format!(
            "Replayed history was rebuilt: {replayed} events, snapshot {snapshot:?}, resumed {resumed:?}"
        )
        .into());
    }
    println!("replayed history: nothing rebuilt, the resumed child is announced first");
    Ok(())
}

fn live(folder: PathBuf, prompt: &str) -> Result<(), Failure> {
    let agent = provider_agent(Provider::Codex).ok_or("Codex has no built-in agent")?;
    let key = (1, 1);
    let mut runtimes = ThreadRuntimes::default();
    let inputs = runtimes.start(key, &folder.to_string_lossy(), prompt);
    runtimes.worktree_ready(key);
    let setup = SessionSetup::new(folder, agent.bypass_modes);
    let mut session = spawn_session(agent.launch, setup, inputs);
    while let Some(event) = session.events.blocking_recv() {
        runtimes.apply(key, &event);
        match &event {
            AgentEvent::Subagent { spawn, update, .. } => {
                println!("subagent {spawn}: {}", update_kind(update)?);
            }
            AgentEvent::SubagentSession { spawn, update } => {
                println!("about {spawn}: {}", serde_json::to_string(update)?);
            }
            AgentEvent::TurnEnded(outcome) => {
                println!("turn ended: {outcome:?}");
                break;
            }
            AgentEvent::Log(line) => eprintln!("agent: {line}"),
            _ => {}
        }
    }
    let view = runtimes.view(key).ok_or("Missing thread view")?;
    println!("parent steps: {}", step_verbs(&view.blocks).join(", "));
    view.subagents.iter().for_each(print_subagent);
    if view.subagents.iter().all(|node| node.blocks.is_empty()) {
        return Err("No subagent reported a step of its own".into());
    }
    Ok(())
}

fn update_kind(update: &impl serde::Serialize) -> Result<String, Failure> {
    let update = serde_json::to_value(update)?;
    let field = |name: &str| update.get(name).and_then(Value::as_str).unwrap_or_default();
    Ok(format!("{} {}", field("sessionUpdate"), field("title")))
}

fn print_subagent(subagent: &SubagentView) {
    println!(
        "{} · {:?} · {} · {} steps · {} tokens · now {}",
        subagent.title,
        subagent.status,
        subagent.model.as_deref().unwrap_or("thread's model"),
        subagent.step_count,
        subagent.tokens.unwrap_or_default(),
        subagent.current_step.as_deref().unwrap_or("-"),
    );
}

fn step_verbs(blocks: &[TranscriptBlock]) -> Vec<String> {
    blocks
        .iter()
        .flat_map(|block| match block {
            TranscriptBlock::ToolGroup { steps } => {
                steps.iter().map(|step| step.verb.clone()).collect()
            }
            _ => Vec::new(),
        })
        .filter(|verb| verb != "Worktree")
        .collect()
}

/// One turn in the shapes codex-acp 2.6 sent a client that declared the capability: the
/// spawn is announced rather than shown as a `spawnAgent` call, and the parent's later
/// collab calls stay its own steps.
fn codex_turn() -> Vec<Value> {
    Vec::from([
        notification(ROOT, message("I'll hand the search to a subagent.")),
        about_child(json!({"capabilities": {}, "title": "Trace thread events"})),
        notification(
            CHILD,
            json!({"sessionUpdate": "config_option_update", "configOptions": [
                {"id": "model", "name": "Model", "category": "model", "type": "select",
                    "description": "Model Codex uses for the subagent",
                    "currentValue": "gpt-5.2-codex",
                    "options": [{"value": "gpt-5.2-codex", "name": "GPT-5.2 Codex"}]},
                {"id": "reasoning_effort", "name": "Reasoning effort",
                    "category": "thought_level", "type": "select", "currentValue": "high",
                    "options": [{"value": "high", "name": "High"}]}]}),
        ),
        state(json!({"state": "running"})),
        about_child(json!({"capabilities": {"cancel": {}}})),
        about_child(json!({"cwd": "W:\\work\\thread"})),
        notification(
            ROOT,
            json!({"sessionUpdate": "tool_call", "toolCallId": "p1", "title": "wait",
                "kind": "other", "status": "in_progress",
                "rawInput": {"prompt": null, "senderThreadId": "019a-root",
                    "receiverThreadIds": [], "agentsStates": {}, "status": "inProgress"}}),
        ),
        child_tool("c1", "search", "rg emit_step"),
        notification(
            CHILD,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed"}),
        ),
        child_tool("c2", "read", "Read src-tauri/src/threads.rs"),
        notification(
            CHILD,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c2", "status": "completed"}),
        ),
        notification(
            CHILD,
            message("Events are emitted in src-tauri/src/threads.rs."),
        ),
        notification(
            CHILD,
            json!({"sessionUpdate": "usage_update", "used": 18_250, "size": 272_000}),
        ),
        state(json!({"state": "idle", "stopReason": "end_turn"})),
        about_child(json!({"capabilities": {"cancel": null}})),
        notification(
            ROOT,
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "p1", "status": "completed"}),
        ),
        notification(ROOT, message("The subagent reported back.")),
    ])
}

fn notification(session: &str, update: Value) -> Value {
    json!({"sessionId": session, "update": update})
}

/// A `subagent_update` in the parent session, naming the child.
fn about_child(fields: Value) -> Value {
    let mut update = json!({"sessionUpdate": "subagent_update", "sessionId": CHILD});
    if let (Some(update), Some(fields)) = (update.as_object_mut(), fields.as_object()) {
        update.extend(fields.clone());
    }
    notification(ROOT, update)
}

fn state(state: Value) -> Value {
    about_child(json!({ "state": state }))
}

fn child_tool(identifier: &str, kind: &str, title: &str) -> Value {
    notification(
        CHILD,
        json!({"sessionUpdate": "tool_call", "toolCallId": identifier, "title": title,
            "kind": kind, "status": "in_progress"}),
    )
}

fn message(text: &str) -> Value {
    json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}})
}
