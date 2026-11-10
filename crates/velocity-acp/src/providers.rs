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

/// The ACP agent for `provider` (§3.2): Claude Code through `claude-agent-acp`, Codex through
/// `codex-acp`, Kimi CLI's `kimi acp` and Antigravity's own `agy acp` (Q14), each run with
/// your own config and login.
pub fn provider_agent(provider: Provider) -> ProviderAgent {
    let (program, args, bypass_modes): (&str, &[&str], &[&str]) = match provider {
        Provider::Anthropic => ("claude-agent-acp", &[], &["bypassPermissions"]),
        Provider::Codex => ("codex-acp", &[], &["full-access"]),
        Provider::KimiCode => ("kimi", &["acp"], &[]),
        // `yolo` is Antigravity's unrestricted mode (§3.2 Antigravity, Rule 1).
        Provider::Antigravity => ("agy", &["acp"], &["yolo"]),
    };
    ProviderAgent {
        launch: AgentLaunch {
            program: PathBuf::from(program),
            args: args.iter().map(|argument| (*argument).to_owned()).collect(),
            env: Vec::new(),
        },
        bypass_modes: bypass_modes.iter().map(|mode| (*mode).to_owned()).collect(),
    }
}

/// The command as the Providers card shows it, e.g. `claude-agent-acp · stdio` (F15).
pub fn agent_command_label(launch: &AgentLaunch) -> String {
    let mut parts = vec![launch.program.display().to_string()];
    parts.extend(launch.args.iter().cloned());
    format!("{} · stdio", parts.join(" "))
}
