$ErrorActionPreference = 'Stop'
# F4: interview tool steps lose skill identity when ACP supplies a path or raw input.
$root = Split-Path -Parent $PSScriptRoot
$fixture = Join-Path $env:TEMP ('velocity-pr163-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path (Join-Path $fixture 'src') -Force | Out-Null
$crates = (Join-Path $root 'crates').Replace('\', '/')
@"
[package]
name = "pr163-interview-skill-load"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
agent-client-protocol = "=5.0.0"
velocity-core = { path = "$crates/velocity-core" }
velocity-acp = { path = "$crates/velocity-acp" }
velocity-interview = { path = "$crates/velocity-interview" }
serde_json = "1"
"@ | Set-Content -LiteralPath (Join-Path $fixture 'Cargo.toml')
@'
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
'@ | Set-Content -LiteralPath (Join-Path $fixture 'src/main.rs')
$env:CARGO_TARGET_DIR = Join-Path $root 'target'
& (Join-Path $root 'scripts/build-queue.ps1') cargo run --quiet --manifest-path (Join-Path $fixture 'Cargo.toml')
exit $LASTEXITCODE

