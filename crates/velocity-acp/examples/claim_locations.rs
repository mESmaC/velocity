//! Headless ACP mutation-location driver, including partial updates.
use agent_client_protocol::v1::schema::SessionUpdate;
use velocity_acp::tool_mutations::ToolMutations;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut tools = ToolMutations::default();
    for (kind, expected) in [
        ("read", 0),
        ("search", 0),
        ("execute", 0),
        ("other", 0),
        ("edit", 1),
        ("delete", 1),
        ("move", 1),
    ] {
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":kind, "title":kind,
            "kind":kind,"locations":[{"path":"AGENTS.md"}]
        }))?;
        let paths = tools.update(&update);
        if paths.len() != expected {
            return Err(format!("{kind}: unexpected {paths:?}").into());
        }
        let update: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call_update", "toolCallId":kind,
            "locations":[{"path":"SPEC.md"}]
        }))?;
        if tools.update(&update).len() != expected {
            return Err(format!("{kind}: partial update lost kind").into());
        }
        println!("{kind}: {expected} claimed locations, including partial update");
    }
    let unknown: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call_update", "toolCallId":"unknown", "locations":[{"path":"SPEC.md"}]
    }))?;
    if !tools.update(&unknown).is_empty() {
        return Err("unclassified update claimed a file".into());
    }
    let classified: SessionUpdate = serde_json::from_value(serde_json::json!({
        "sessionUpdate":"tool_call_update", "toolCallId":"unknown", "kind":"edit"
    }))?;
    if tools.update(&classified).len() != 1 {
        return Err("classification lost earlier locations".into());
    }
    let write = velocity_acp::client::AgentEvent::FileWritten {
        path: "written.rs".into(),
    };
    if tools.observe(&write).len() != 1 {
        return Err("explicit write was ignored".into());
    }
    verify_read_claims()?;
    Ok(())
}

fn verify_read_claims() -> Result<(), Box<dyn std::error::Error>> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("velocity-claim-locations-{stamp}"));
    let channel = velocity_airc::Channel::open(&directory, "project")?;
    for member in ["thread-1", "thread-2"] {
        channel.join(member, member)?;
        channel.claim(member, member, &format!("vel/{member}"), Vec::new())?;
        let mut tools = ToolMutations::default();
        let read: SessionUpdate = serde_json::from_value(serde_json::json!({
            "sessionUpdate":"tool_call", "toolCallId":"read", "title":"Read instructions",
            "kind":"read", "locations":[{"path":"AGENTS.md"},{"path":"SPEC.md"}]
        }))?;
        for path in tools.update(&read) {
            channel.observe_file(member, &path.display().to_string())?;
        }
    }
    if channel.read(0)?.iter().any(|entry| entry.kind == "overlap") {
        return Err("two instruction reads generated an overlap".into());
    }
    println!("two agents reading instructions: no file claims or overlaps");
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
