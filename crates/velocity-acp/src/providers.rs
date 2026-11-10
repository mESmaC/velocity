//! Which command runs each provider's ACP agent (SPEC.md §3.2 Provider code). This is the
//! only provider-specific part of a session.

use std::path::PathBuf;

use crate::Provider;
use crate::client::AgentLaunch;

/// How one provider's agent starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAgent {
    pub launch: AgentLaunch,
    /// The agent's session modes that skip its permission checks (Rule 1). Permission
    /// requests are answered allow-always either way; the mode only saves the round trip.
    pub bypass_modes: Vec<String>,
}

/// The ACP agent for `provider`, or `None` while its agent is undecided (Antigravity, Q14).
pub fn provider_agent(provider: Provider) -> Option<ProviderAgent> {
    let (program, args, bypass_modes): (&str, &[&str], &[&str]) = match provider {
        Provider::Anthropic => ("claude-agent-acp", &[], &["bypassPermissions"]),
        Provider::Codex => ("codex-acp", &[], &[]),
        Provider::KimiCode => ("kimi", &["acp"], &[]),
        Provider::Antigravity => return None,
    };
    Some(ProviderAgent {
        launch: AgentLaunch {
            program: PathBuf::from(program),
            args: args.iter().map(|argument| (*argument).to_owned()).collect(),
            env: Vec::new(),
        },
        bypass_modes: bypass_modes.iter().map(|mode| (*mode).to_owned()).collect(),
    })
}
