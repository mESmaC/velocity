//! Reads every provider the way Settings → Providers does (F15): each agent's sign-in state,
//! sign-in methods and models over ACP, and the Claude Code and Codex limit reports.
//!
//! `cargo run -p velocity-acp --example providers -- <directory> [<program> <arguments>…]`

use std::path::PathBuf;
use std::time::Duration;

use velocity_acp::client::{probe_agent, read_claude_usage, read_codex_usage};
use velocity_acp::{Provider, agent_command_label, choose_sign_in, configured_agent};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let directory = std::env::args()
        .nth(1)
        .map_or_else(std::env::temp_dir, PathBuf::from);
    // Any arguments after the directory are a custom agent's command (F15 Custom ACP
    // agents), probed after the four: `providers <dir> <program> [arguments…]`.
    let custom = std::env::args().nth(2).is_some();
    let providers = [
        Provider::Anthropic,
        Provider::Codex,
        Provider::Antigravity,
        Provider::KimiCode,
        Provider::Custom(1),
    ];
    for provider in providers {
        if custom != matches!(provider, Provider::Custom(_)) {
            continue;
        }
        let Some(agent) = configured_agent(provider, custom_command(provider)) else {
            continue;
        };
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

/// The custom agent's command from the arguments after the directory.
fn custom_command(provider: Provider) -> Option<(PathBuf, Vec<String>)> {
    if !matches!(provider, Provider::Custom(_)) {
        return None;
    }
    let mut words = std::env::args().skip(2);
    let program = words.next()?;
    Some((PathBuf::from(program), words.collect()))
}
