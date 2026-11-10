//! Reads every provider the way Settings → Providers does (F15): each agent's sign-in state,
//! sign-in methods and models over ACP, and the Claude Code and Codex limit reports.
//!
//! `cargo run -p velocity-acp --example providers -- <directory>`

use std::path::PathBuf;
use std::time::Duration;

use velocity_acp::client::{probe_agent, read_claude_usage, read_codex_usage};
use velocity_acp::{Provider, agent_command_label, choose_sign_in, provider_agent};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let directory = std::env::args()
        .nth(1)
        .map_or_else(std::env::temp_dir, PathBuf::from);
    for provider in [
        Provider::Anthropic,
        Provider::Codex,
        Provider::Antigravity,
        Provider::KimiCode,
    ] {
        let agent = provider_agent(provider);
        println!("{provider:?} · {}", agent_command_label(&agent.launch));
        match probe_agent(&agent.launch, &directory, Duration::from_secs(90)).await {
            Ok(probe) => {
                let chosen = choose_sign_in(&probe.sign_in_methods)
                    .and_then(|index| probe.sign_in_methods.get(index));
                println!("  signed in: {}", probe.signed_in);
                println!("  sign in with: {:?}", chosen.map(|method| &method.id));
                for model in &probe.models {
                    let fast = if model.fast { " · fast" } else { "" };
                    println!("  model {} ({}){fast}", model.name, model.id);
                }
            }
            Err(error) => println!("  {error}"),
        }
    }
    let limit = Duration::from_secs(60);
    println!(
        "Anthropic limit: {:?}",
        read_claude_usage(&directory, limit).await
    );
    println!(
        "Codex limit: {:?}",
        read_codex_usage(&directory, limit).await
    );
}
