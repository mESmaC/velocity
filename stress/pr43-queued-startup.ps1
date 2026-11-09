# Risk: steering queued while worktree setup is running must remain visibly
# queued, and its following turn must not be published as idle (Rule 1, F4).
# Uses the actual client steering policy and the new thread runtime projection.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr43-queued-startup'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$threadsPath = (Join-Path $repositoryRoot 'crates\velocity-threads').Replace('\', '/')
$acpPath = (Join-Path $repositoryRoot 'crates\velocity-acp').Replace('\', '/')
$storePath = (Join-Path $repositoryRoot 'crates\velocity-store').Replace('\', '/')
$corePath = (Join-Path $repositoryRoot 'crates\velocity-core').Replace('\', '/')
$manifest = @"
[package]
name = "pr43-queued-startup"
version = "0.0.0"
edition = "2024"
[workspace]
[dependencies]
velocity-threads = { path = "$threadsPath" }
velocity-acp = { path = "$acpPath" }
velocity-store = { path = "$storePath" }
velocity-core = { path = "$corePath" }
agent-client-protocol = "5.0.0"
"@
$source = @'
use agent_client_protocol::v1::schema::{ContentBlock, ContentChunk, SessionUpdate, StopReason};
use velocity_acp::{Placement, Steering, SteeringEffect, SteeringInput, client::{AgentEvent, AgentInput}};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::AgentActivity;
use velocity_threads::{ThreadKey, ThreadRuntimes};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn queued(runtime: &ThreadRuntimes, key: ThreadKey) -> bool {
    runtime.view(key).expect("live view").blocks.iter().any(|block| matches!(
        block, TranscriptBlock::UserMessage {text, queued: true} if text == "queued during setup"))
}

fn message(index: usize, text: &str, agent: bool) -> AgentEvent {
    let chunk = ContentChunk::new(ContentBlock::from(text.to_owned()));
    AgentEvent::Transcript {placement: Placement::Added(index), update: if agent {
        SessionUpdate::AgentMessageChunk(chunk)
    } else { SessionUpdate::UserMessageChunk(chunk) }}
}

fn main() {
    let key = (1, 1);
    let mut runtime = ThreadRuntimes::default();
    let mut inputs = runtime.start(key, r"C:\isolated fixture\worktree", "initial prompt");
    runtime.steer(key, "queued during setup").expect("queue steering before agent starts");
    let mut failures = report("queued-visible-during-setup", queued(&runtime, key), "second message is pending");
    runtime.worktree_ready(key);
    let mut steering: Steering<Vec<ContentBlock>, ()> = Steering::new(());
    let AgentInput::Send(first) = inputs.try_recv().expect("first input") else { panic!("wrong first input") };
    assert!(matches!(steering.step(SteeringInput::Send(first)), SteeringEffect::Prompt(_)));
    let AgentInput::Send(second) = inputs.try_recv().expect("second input") else { panic!("wrong second input") };
    assert!(matches!(steering.step(SteeringInput::Send(second)), SteeringEffect::Queued));
    runtime.apply(key, &message(0, "initial prompt", false));
    runtime.apply(key, &AgentEvent::Queued);
    failures += report("queued-visible-after-initial-prompt-echo", queued(&runtime, key), format!("client queue has {} message", steering.queued().len()));
    runtime.apply(key, &message(1, "first turn response", true));
    let progress = runtime.apply(key, &AgentEvent::TurnEnded(Ok(StopReason::EndTurn))).expect("turn progress");
    let next = steering.step(SteeringInput::TurnEnded);
    let delivered = matches!(next, SteeringEffect::Prompt(ref messages) if messages.len() == 1);
    failures += report("client-delivers-the-queued-message", delivered && steering.is_busy(), "next prompt runs; nothing was dropped by the ACP queue");
    failures += report("followup-turn-remains-thinking", progress.activity == AgentActivity::Thinking, format!("expected=Thinking, actual={:?}", progress.activity));
    let progress = runtime.apply(key, &message(2, "queued during setup", false)).expect("followup progress");
    failures += report("followup-prompt-is-not-idle", progress.activity != AgentActivity::Idle, format!("client busy={}, actual={:?}", steering.is_busy(), progress.activity));
    println!("failures: {failures}");
    std::process::exit(i32::from(failures != 0));
}
'@
$encoding = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'Cargo.toml'), $manifest, $encoding)
[IO.File]::WriteAllText((Join-Path $harnessRoot 'src\main.rs'), $source, $encoding)
Copy-Item -LiteralPath (Join-Path $repositoryRoot 'Cargo.lock') -Destination (Join-Path $harnessRoot 'Cargo.lock')
$env:CARGO_TARGET_DIR = Join-Path $repositoryRoot '.tools\pr43-target'
cargo run --manifest-path (Join-Path $harnessRoot 'Cargo.toml') --offline
exit $LASTEXITCODE
