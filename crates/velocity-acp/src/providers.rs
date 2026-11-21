//! Which command runs each provider's ACP agent (SPEC.md §3.2 Provider code). This is the
//! only provider-specific part of a session.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{Value, json};

use crate::Provider;
use crate::client::{AgentLaunch, hidden_command};

/// How one provider's agent starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderAgent {
    pub launch: AgentLaunch,
    /// The agent's session modes that skip its permission checks (Rule 1). Permission
    /// requests are answered allow-always either way; the mode only saves the round trip.
    pub bypass_modes: Vec<String>,
}

impl ProviderAgent {
    /// Trim provider extras in this child process only (F13, #458).
    pub fn trim_unused_context(&mut self) {
        match self.launch.own_config {
            Some(Provider::Anthropic) => {
                for (name, value) in [
                    ("ENABLE_CLAUDEAI_MCP_SERVERS", "false"),
                    ("CLAUDE_CODE_DISABLE_BUNDLED_SKILLS", "1"),
                    ("CLAUDE_CODE_DISABLE_ARTIFACT", "1"),
                    ("CLAUDE_CODE_DISABLE_WORKFLOWS", "1"),
                ] {
                    self.launch.set_env(name, value);
                }
                if std::env::var_os("ENABLE_TOOL_SEARCH").is_none() {
                    self.launch.set_env("ENABLE_TOOL_SEARCH", "true");
                }
            }
            Some(Provider::Codex) => trim_codex_context(&mut self.launch),
            _ => {}
        }
    }
}

/// Whether `provider`'s harness natively supports on-demand/deferred MCP tool loading (#533).
pub const fn supports_lazy_tools(provider: Option<Provider>) -> bool {
    matches!(provider, Some(Provider::Anthropic | Provider::Antigravity))
}

fn trim_codex_context(launch: &mut AgentLaunch) {
    // codex-acp merges CODEX_CONFIG into new and resumed sessions. Keep any settings
    // supplied by the caller and leave the account's own configuration on disk.
    const CONFIG: &str = "CODEX_CONFIG";
    let inherited = std::env::var(CONFIG).ok();
    let mut config = match launch.env_value(CONFIG).or(inherited.as_deref()) {
        None | Some("") => json!({}),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(value) if value.is_object() => value,
            _ => return,
        },
    };
    merge_config(
        &mut config,
        json!({
            "skills": { "include_instructions": false },
            "features": { "apps": false },
        }),
    );
    launch.set_env(CONFIG, &config.to_string());
}

const CODEX_PATH: &str = "CODEX_PATH";
const CODEX_ADAPTER: &str = "VELOCITY_CODEX_ADAPTER";
const ORIGINAL_CODEX_PATH: &str = "VELOCITY_ORIGINAL_CODEX_PATH";
static APP_EXECUTABLE: OnceLock<PathBuf> = OnceLock::new();

pub fn enable_codex_startup_override(executable: PathBuf) {
    let _ = APP_EXECUTABLE.set(executable);
}

pub(crate) fn configure_codex_startup(
    command: &mut tokio::process::Command,
    launch: &AgentLaunch,
    program: &Path,
) {
    if launch.own_config != Some(Provider::Codex) {
        return;
    }
    let Some(executable) = APP_EXECUTABLE.get() else {
        return;
    };
    let original = launch
        .env_value(CODEX_PATH)
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os(CODEX_PATH))
        .unwrap_or_default();
    command
        .env(CODEX_PATH, executable)
        .env(CODEX_ADAPTER, program)
        .env(ORIGINAL_CODEX_PATH, original);
}

/// codex-acp starts App Server before applying CODEX_CONFIG to any session.
/// Use its CLI passthrough to select the same bundled executable with startup overrides.
pub fn serve_codex_app_server() -> std::io::Result<Option<i32>> {
    let Some(adapter) = std::env::var_os(CODEX_ADAPTER) else {
        return Ok(None);
    };
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_none_or(|arg| arg != "app-server") {
        return Ok(None);
    }
    let adapter = PathBuf::from(adapter);
    let original = std::env::var_os(ORIGINAL_CODEX_PATH).filter(|path| !path.is_empty());
    let mut command = if let Some(program) = original {
        hidden_command(program).into_std()
    } else {
        bundled_cli(&adapter)
    };
    command
        .env_remove(CODEX_PATH)
        .env_remove(CODEX_ADAPTER)
        .env_remove(ORIGINAL_CODEX_PATH)
        .args(args)
        .args([
            "-c",
            "features.computer_use=false",
            "-c",
            r#"plugins."unified-computer-use@openai-bundled".enabled=false"#,
            "-c",
            r#"plugins."computer-use@openai-bundled".enabled=false"#,
        ]);
    Ok(Some(command.status()?.code().unwrap_or(1)))
}

fn bundled_cli(adapter: &Path) -> std::process::Command {
    if let Some(script) = adapter
        .parent()
        .map(|parent| parent.join("node_modules/@agentclientprotocol/codex-acp/dist/index.js"))
        .filter(|script| script.is_file())
    {
        let mut command = hidden_command("node").into_std();
        command.arg(script).arg("cli");
        return command;
    }
    let mut command = hidden_command(adapter).into_std();
    command.arg("cli");
    command
}

fn merge_config(config: &mut Value, overrides: Value) {
    match (config, overrides) {
        (Value::Object(config), Value::Object(overrides)) => {
            for (key, value) in overrides {
                merge_config(config.entry(key).or_insert(Value::Null), value);
            }
        }
        (config, value) => *config = value,
    }
}

/// The unrestricted session modes the built-in agents name (Rule 1). An agent you added may
/// be a build of one of them, or share their naming, so it is offered all of them.
const KNOWN_BYPASS_MODES: [&str; 4] = [
    "bypassPermissions",
    "agent-full-access",
    "full-access",
    "yolo",
];

/// codex-acp's unrestricted mode: no sandbox, no approvals. Newer builds name it
/// `agent-full-access` and start every session in `agent` (a workspace-write sandbox that
/// ignores `sandbox_mode` in your Codex config); older builds name it `full-access`. Without
/// it, Codex runs each shell command inside the Windows sandbox, which hangs in worktrees
/// (#301).
const CODEX_FULL_ACCESS: [&str; 2] = ["agent-full-access", "full-access"];

/// The built-in ACP agent for `provider` (§3.2): Claude Code through `claude-agent-acp`,
/// Codex through `codex-acp`, Kimi CLI's `kimi acp` and Antigravity's own `agy acp` (Q14),
/// each run with your own config and login. An agent you added has no built-in command.
pub fn provider_agent(provider: Provider) -> Option<ProviderAgent> {
    let (program, args, bypass_modes): (&str, &[&str], &[&str]) = match provider {
        Provider::Anthropic => ("claude-agent-acp", &[], &["bypassPermissions"]),
        Provider::Codex => ("codex-acp", &[], &CODEX_FULL_ACCESS),
        Provider::KimiCode => ("kimi", &["acp"], &[]),
        // Antigravity CLI 1.5 has no unrestricted session mode (its modes are `default`,
        // `accept-edits`, `auto` and `plan`, and `yolo` is gone), so a session that asked for
        // one failed to start. The launch flag approves every tool call instead (Rule 1).
        Provider::Antigravity => ("agy", &["acp", "--dangerously-skip-permissions"], &[]),
        Provider::Custom(_) => return None,
    };
    let mut launch = AgentLaunch::new(PathBuf::from(program));
    launch.args = args.iter().map(|argument| (*argument).to_owned()).collect();
    launch.own_config = Some(provider);
    launch.concurrency_provider = Some(provider);
    if provider == Provider::Codex {
        // codex-acp reads its first mode from here, so a session is never sandboxed, even
        // before `session/set_mode` arrives or when a resumed session reports no modes.
        launch.env.push((
            "INITIAL_AGENT_MODE".to_owned(),
            CODEX_FULL_ACCESS[0].to_owned(),
        ));
    }
    Some(ProviderAgent {
        launch,
        bypass_modes: bypass_modes.iter().map(|mode| (*mode).to_owned()).collect(),
    })
}

/// The agent `provider` runs: `command` (a program and its arguments) when you set one, in
/// place of the built-in command (F15). A built-in provider keeps its own bypass mode; an
/// agent you added is offered every known one. `None` for an added agent with no command.
pub fn configured_agent(
    provider: Provider,
    command: Option<(PathBuf, Vec<String>)>,
) -> Option<ProviderAgent> {
    let built_in = provider_agent(provider);
    match (built_in, command) {
        (Some(mut agent), Some((program, args))) => {
            agent.launch.program = program;
            agent.launch.args = args;
            Some(agent)
        }
        (Some(agent), None) => Some(agent),
        (None, Some((program, args))) => Some(ProviderAgent {
            launch: AgentLaunch {
                args,
                ..AgentLaunch::new(program)
            },
            bypass_modes: KNOWN_BYPASS_MODES
                .iter()
                .map(|mode| (*mode).to_owned())
                .collect(),
        }),
        (None, None) => None,
    }
    .map(|mut agent| {
        agent.launch.concurrency_provider = Some(provider);
        agent
    })
}

/// The command as the Providers card shows it, e.g. `claude-agent-acp · stdio` (F15).
pub fn agent_command_label(launch: &AgentLaunch) -> String {
    format!("{} · stdio", agent_command_line(launch))
}

/// The program and its arguments as one line, quoting any word with a space, e.g.
/// `"C:\Program Files\Agent\agent.exe" acp`.
pub fn agent_command_line(launch: &AgentLaunch) -> String {
    std::iter::once(launch.program.display().to_string())
        .chain(launch.args.iter().cloned())
        .map(|word| {
            if word.is_empty() || word.contains(char::is_whitespace) {
                format!("\"{word}\"")
            } else {
                word
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
