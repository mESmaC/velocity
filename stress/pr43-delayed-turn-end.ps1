# Risk: queued steering and activity must agree with the ACP client even when
# native event delivery lags behind two UI sends (Rule 1, F4).
# Calls the real runtime and steering policy; no provider process is started.
$ErrorActionPreference = 'Stop'
$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$harnessRoot = Join-Path $repositoryRoot '.tools\pr43-delayed-turn-end'
New-Item -ItemType Directory -Path (Join-Path $harnessRoot 'src') -Force | Out-Null
$threadsPath = (Join-Path $repositoryRoot 'crates\velocity-threads').Replace('\', '/')
$acpPath = (Join-Path $repositoryRoot 'crates\velocity-acp').Replace('\', '/')
$storePath = (Join-Path $repositoryRoot 'crates\velocity-store').Replace('\', '/')
$corePath = (Join-Path $repositoryRoot 'crates\velocity-core').Replace('\', '/')
$manifest = @"
[package]
name = "pr43-delayed-turn-end"
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
use velocity_acp::{EntryClass, MessageKey, Transcript, Steering, SteeringEffect, SteeringInput, client::{AgentEvent, AgentInput}};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::AgentActivity;
use velocity_threads::{ThreadKey, ThreadRuntimes};

fn report(name: &str, passed: bool, detail: impl std::fmt::Display) -> usize {
    println!("{} {name}: {detail}", if passed { "PASS" } else { "FAIL" });
    usize::from(!passed)
}

fn message(transcript: &mut Transcript<String, SessionUpdate>, text: &str) -> AgentEvent {
    let update = SessionUpdate::UserMessageChunk(ContentChunk::new(ContentBlock::from(text.to_owned())));
    let placement = transcript.record(EntryClass::UserMessage(MessageKey::Unnamed), update.clone());
    AgentEvent::Transcript { placement, update }
}

fn queued(runtime: &ThreadRuntimes, key: ThreadKey, wanted: &str) -> bool {
    runtime.view(key).expect("live view").blocks.iter().any(|block| matches!(
        block, TranscriptBlock::UserMessage { text, queued: true } if text == wanted))
}

fn main() {
    let key = (1, 1);
    let mut runtime = ThreadRuntimes::default();
    let mut inputs = runtime.start(key, r"C:\isolated fixture\worktree", "A");
    let mut client: Steering<Vec<ContentBlock>, ()> = Steering::new(());
    let mut transcript = Transcript::new();
    let AgentInput::Send(first) = inputs.try_recv().expect("A input") else { panic!("expected send") };
    assert!(matches!(client.step(SteeringInput::Send(first)), SteeringEffect::Prompt(_)));
    runtime.apply(key, &message(&mut transcript, "A"));

    // drive() emits TurnEnded before stepping the policy. Its separate native
    // event consumer has not applied that event yet; the client is already idle.
    let ended_a = AgentEvent::TurnEnded(Ok(StopReason::EndTurn));
    assert!(matches!(client.step(SteeringInput::TurnEnded), SteeringEffect::Nothing));
    runtime.steer(key, "B").expect("send B while the event pump lags");
    let AgentInput::Send(second) = inputs.try_recv().expect("B input") else { panic!("expected send") };
    assert!(matches!(client.step(SteeringInput::Send(second)), SteeringEffect::Prompt(_)));
    runtime.steer(key, "C").expect("send C during B");
    let AgentInput::Send(third) = inputs.try_recv().expect("C input") else { panic!("expected send") };
    assert!(matches!(client.step(SteeringInput::Send(third)), SteeringEffect::Queued));

    // Deliver the buffered client events in their actual FIFO order. The client
    // still queues C behind B; no event reordering or message loss is simulated.
    runtime.apply(key, &ended_a);
    runtime.apply(key, &message(&mut transcript, "B"));
    runtime.apply(key, &AgentEvent::Queued);
    let mut failures = report("client-still-queues-C", client.queued().len() == 1, "C waits behind active B");
    failures += report("C-remains-visibly-queued", queued(&runtime, key, "C"), "delivering A's end must not consume C");
    let ended_b = AgentEvent::TurnEnded(Ok(StopReason::EndTurn));
    let delivered = matches!(client.step(SteeringInput::TurnEnded), SteeringEffect::Prompt(ref sent) if sent.len() == 1);
    failures += report("client-starts-C", delivered && client.is_busy(), "C is delivered, without interruption");
    runtime.apply(key, &ended_b);
    let progress = runtime.apply(key, &message(&mut transcript, "C")).expect("C progress");
    failures += report("C-is-thinking-while-client-busy", progress.activity == AgentActivity::Thinking,
        format!("client busy={}, actual={:?}", client.is_busy(), progress.activity));
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
