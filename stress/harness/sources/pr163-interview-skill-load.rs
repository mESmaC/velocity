use agent_client_protocol::v1::schema::{SessionUpdate, ToolCall};
use velocity_acp::Placement;
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_interview::live::LiveTranscript;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut failures = 0;
    for (label, input) in [
        ("location", serde_json::json!({"toolCallId":"1", "title":"Read SKILL.md", "kind":"read", "status":"completed", "locations":[{"path":"X:\\project\\.agents\\skills\\ui-ux-pro-max\\SKILL.md"}]})),
        ("raw input", serde_json::json!({"toolCallId":"2", "title":"Skill", "status":"completed", "rawInput":{"skill":"ui-ux-pro-max"}})),
    ] {
        let call: ToolCall = serde_json::from_value(input)?;
        let mut live = LiveTranscript::default();
        live.record(Placement::Added(0), &SessionUpdate::ToolCall(call));
        for block in live.blocks() {
            if let TranscriptBlock::ToolGroup { steps } = block {
                for step in steps {
                    println!("{label}: {} | {}", step.verb, step.argument);
                    if step.verb != "Skill" || step.argument != "ui-ux-pro-max" { failures += 1; }
                }
            }
        }
    }
    if failures > 0 { return Err(format!("{failures} skill loads missing Skill label").into()); }
    Ok(())
}
