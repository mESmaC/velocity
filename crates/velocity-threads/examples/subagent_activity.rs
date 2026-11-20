//! Replays what claude-agent-acp sends for a turn with two parallel subagents (#436) through
//! the ACP client's routing and a thread runtime, without starting a provider process.
//!
//! The updates use the agent's wire shapes: each `Agent` call names itself in
//! `_meta.claudeCode.toolName`, and every update made inside it carries
//! `_meta.claudeCode.parentToolUseId`, except a later report of a tool call that omits the
//! stamp, which stays with the subagent the call's first update named.
use agent_client_protocol::v1::schema::SessionUpdate;
use serde_json::{Value, json};
use velocity_acp::client::{AgentEvent, entry_class};
use velocity_acp::{SubagentRouter, Transcript};
use velocity_core::thread_view_model::{SubagentStatus, SubagentView, TranscriptBlock};
use velocity_threads::ThreadRuntimes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let key = (1, 1);
    let mut runtimes = ThreadRuntimes::default();
    let _inputs = runtimes.start(key, "W:\\work\\thread", "Investigate");
    runtimes.worktree_ready(key);
    let mut router = SubagentRouter::new();
    let mut parent = Transcript::<std::sync::Arc<str>, ()>::new();
    for update in claude_turn() {
        let update: SessionUpdate = serde_json::from_value(update)?;
        let class = entry_class(&update).ok_or("Update outside the transcript")?;
        // The same routing `Shared::record_subagent` does in the ACP client.
        let event = match router.route(&update, &class) {
            Some((spawn, placement)) => AgentEvent::Subagent {
                spawn,
                placement,
                update,
            },
            None => AgentEvent::Transcript {
                placement: parent.record(class, ()),
                update,
            },
        };
        runtimes.apply(key, &event);
    }
    let view = runtimes.view(key).ok_or("Missing thread view")?;
    check_parent(&view.blocks)?;
    check_subagent(
        &view.subagents,
        0,
        "Trace event delivery",
        &["Read", "Edit"],
    )?;
    check_subagent(&view.subagents, 1, "Inspect store save path", &["Search"])?;
    let changed = runtimes.take_changed_subagents(key);
    if changed.len() != 2 || view.subagents[1].model.as_deref() != Some("sonnet") {
        return Err("Subagent changes or the requested model were lost".into());
    }
    for subagent in &view.subagents {
        println!(
            "{} · {:?} · {} steps · {} tokens · now {}",
            subagent.title,
            subagent.status,
            subagent.step_count,
            subagent.tokens.unwrap_or_default(),
            subagent.current_step.as_deref().unwrap_or("-"),
        );
    }
    println!("parent steps: {}", step_verbs(&view.blocks).join(", "));
    Ok(())
}

/// The parent keeps only its own two `Agent` calls (kind `think`) and messages.
fn check_parent(blocks: &[TranscriptBlock]) -> Result<(), Box<dyn std::error::Error>> {
    let steps = step_verbs(blocks);
    if steps != ["Think", "Think"] {
        return Err(format!("The parent shows subagent steps: {steps:?}").into());
    }
    Ok(())
}

fn check_subagent(
    subagents: &[SubagentView],
    index: usize,
    title: &str,
    verbs: &[&str],
) -> Result<(), Box<dyn std::error::Error>> {
    let subagent = subagents.get(index).ok_or("Missing subagent")?;
    let steps = step_verbs(&subagent.blocks);
    if subagent.title != title
        || steps != verbs
        || subagent.status != SubagentStatus::Done
        || subagent.tokens.is_none()
    {
        return Err(format!("Subagent {index} is wrong: {subagent:?}").into());
    }
    Ok(())
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

fn claude_turn() -> Vec<Value> {
    Vec::from([
        message(None, "Running two subagents in parallel."),
        spawn("toolu_A", "Trace event delivery", None),
        spawn("toolu_B", "Inspect store save path", Some("sonnet")),
        tool("toolu_A", "a1", "read", "Read ui/src/lib/ipc.ts"),
        tool("toolu_B", "b1", "search", "grep save_state"),
        // A later report of the subagent's call without `parentToolUseId`.
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "a1", "status": "completed",
            "_meta": {"claudeCode": {"toolName": "Read"}}}),
        message(Some("toolu_B"), "The save path is in velocity-store."),
        json!({"sessionUpdate": "tool_call", "toolCallId": "a2", "title": "Edit ipc.ts",
            "kind": "edit", "status": "completed",
            "content": [{"type": "diff", "path": "W:\\work\\thread\\ui\\src\\lib\\ipc.ts",
                "oldText": "a\n", "newText": "b\n"}],
            "_meta": {"claudeCode": {"toolName": "Edit", "parentToolUseId": "toolu_A"}}}),
        json!({"sessionUpdate": "tool_call_update", "toolCallId": "b1", "status": "completed",
            "_meta": {"claudeCode": {"toolName": "Grep", "parentToolUseId": "toolu_B"}}}),
        finish("toolu_A", 1834),
        finish("toolu_B", 942),
        message(None, "Both subagents reported back."),
    ])
}

fn spawn(identifier: &str, description: &str, model: Option<&str>) -> Value {
    json!({"sessionUpdate": "tool_call", "toolCallId": identifier, "title": description,
        "kind": "think", "status": "in_progress",
        "rawInput": {"description": description, "prompt": "Investigate.",
            "subagent_type": "Explore", "model": model},
        "_meta": {"claudeCode": {"toolName": "Agent"}}})
}

fn tool(spawn: &str, identifier: &str, kind: &str, title: &str) -> Value {
    json!({"sessionUpdate": "tool_call", "toolCallId": identifier, "title": title,
        "kind": kind, "status": "in_progress",
        "_meta": {"claudeCode": {"toolName": "Read", "parentToolUseId": spawn}}})
}

fn message(spawn: Option<&str>, text: &str) -> Value {
    let meta = spawn.map(|spawn| json!({"claudeCode": {"parentToolUseId": spawn}}));
    json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text},
        "_meta": meta})
}

fn finish(identifier: &str, tokens: u64) -> Value {
    json!({"sessionUpdate": "tool_call_update", "toolCallId": identifier, "status": "completed",
        "_meta": {"claudeCode": {"toolName": "Agent",
            "toolResponse": {"status": "completed", "totalTokens": tokens}}}})
}
