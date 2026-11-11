//! Run a real provider turn and print only its live subscription readings.
//! `cargo run -p velocity-acp --example live_limits -- claude|codex`
use agent_client_protocol::v1::schema::{ContentBlock, TextContent};
use tokio::sync::mpsc::unbounded_channel;
use velocity_acp::client::{AgentEvent, AgentInput, SessionSetup, run_session, subscribe_limits};
use velocity_acp::{Provider, provider_agent};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = match std::env::args().nth(1).as_deref() {
        Some("claude") => Provider::Anthropic,
        Some("codex") => Provider::Codex,
        _ => return Err("choose claude or codex".into()),
    };
    let agent = provider_agent(provider).ok_or("no agent")?;
    let setup = SessionSetup::new(std::env::current_dir()?, agent.bypass_modes);
    let mut reports = subscribe_limits();
    let (inputs, receiver) = unbounded_channel();
    let (events, mut received) = unbounded_channel();
    inputs.send(AgentInput::Send(vec![ContentBlock::Text(
        TextContent::new("Reply with only done. Do not use tools or change files."),
    )]))?;
    let session = tokio::spawn(run_session(agent.launch, setup, receiver, events));
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        loop {
            tokio::select! {
                report = reports.recv() => println!("limit: {:?}", report?),
                event = received.recv() => match event {
                    Some(AgentEvent::TurnEnded(outcome)) => {
                        println!("turn: {outcome:?}");
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    })
    .await;
    inputs.send(AgentInput::Stop)?;
    session.await??;
    outcome??;
    while let Ok(report) = reports.try_recv() {
        println!("limit: {report:?}");
    }
    Ok(())
}
