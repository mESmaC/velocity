//! Launching an agent: one process per thread, in that thread's worktree (§3.2).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use agent_client_protocol::v1::schema::McpServer;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::process::Command;

use super::ClientError;
use super::program_path::{find_program, search_path};
use crate::Provider;

/// The command that runs one provider's ACP agent. This is the only provider-specific part
/// of a session (§3.2 Provider code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLaunch {
    /// The agent's executable: a path, or a bare name looked up on `PATH`, including `.cmd`
    /// shims such as `claude-agent-acp` on Windows (`program_path::find_program`).
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Added to Velocity's own environment, which the agent inherits whole: your `PATH`,
    /// toolchains, credentials and config (§3.2 Tool access).
    pub env: Vec<(String, String)>,
    /// Settings → Agents → shared MCP servers (§3.2 Tool access), filled by whoever builds
    /// the launch from settings. `run_session` passes them in `session/new`, after Velocity's
    /// own servers and minus the names the agent's own config already has.
    pub shared_servers: Vec<McpServer>,
    /// The built-in provider whose config files the agent loads, so a shared server whose
    /// name is also in them is left to the agent's own entry. `None` for an agent you added.
    pub own_config: Option<Provider>,
    /// Provider slot shared with every role, including sessions using agent defaults.
    pub concurrency_provider: Option<Provider>,
    /// F15 account identifier within the provider. Zero uses the existing CLI login.
    pub account: u32,
}

impl AgentLaunch {
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            args: Vec::new(),
            env: Vec::new(),
            shared_servers: Vec::new(),
            own_config: None,
            concurrency_provider: None,
            account: 0,
        }
    }

    pub fn use_account(&mut self, account: u32, directory: &str) {
        if self.account != 0 && account == 0 {
            self.env.retain(|(name, _)| {
                !matches!(
                    name.as_str(),
                    "CLAUDE_CONFIG_DIR"
                        | "CODEX_HOME"
                        | "KIMI_CODE_HOME"
                        | "KIMI_SHARE_DIR"
                        | "ANTIGRAVITY_GEMINI_DIR"
                )
            });
        }
        self.account = account;
        if account == 0 {
            return;
        }
        match self.own_config {
            Some(Provider::Anthropic) => self.set_env("CLAUDE_CONFIG_DIR", directory),
            Some(Provider::Codex) => self.set_env("CODEX_HOME", directory),
            Some(Provider::KimiCode) => {
                self.set_env("KIMI_CODE_HOME", directory);
                self.set_env("KIMI_SHARE_DIR", directory);
            }
            Some(Provider::Antigravity) => {
                self.set_env("ANTIGRAVITY_GEMINI_DIR", directory);
            }
            _ => {}
        }
    }

    pub(super) fn profile_args(&self) -> impl Iterator<Item = &str> {
        self.antigravity_profile()
            .into_iter()
            .flat_map(|directory| {
                [
                    "--gemini-dir",
                    directory,
                    "--app-data-dir",
                    "antigravity-cli",
                ]
            })
    }

    pub(super) fn antigravity_profile(&self) -> Option<&str> {
        (self.account != 0 && self.own_config == Some(Provider::Antigravity))
            .then(|| self.env_value("ANTIGRAVITY_GEMINI_DIR"))
            .flatten()
    }

    pub(super) fn prepare_account(&self) -> std::io::Result<()> {
        let Some(directory) = self.antigravity_profile() else {
            return Ok(());
        };
        let cache = Path::new(directory).join("antigravity-cli").join("cache");
        std::fs::create_dir_all(&cache)?;
        // agy's composite token store otherwise uses the OS user's shared keyring.
        // Refresh the CLI's native file-storage marker before every isolated launch.
        std::fs::write(cache.join("antigravity-keyring-unavailable"), [])?;
        std::fs::write(cache.join("jetski-cli-keyring-unavailable"), [])
    }

    pub(crate) fn set_env(&mut self, name: &str, value: &str) {
        self.env
            .retain(|(existing, _)| !existing.eq_ignore_ascii_case(name));
        self.env.push((name.to_owned(), value.to_owned()));
    }

    pub(crate) fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(variable, _)| variable == name)
            .map(|(_, value)| value.as_str())
    }

    pub(super) fn apply_environment(&self, command: &mut Command) {
        command.envs(self.env.iter().map(|(name, value)| (name, value)));
        if self.account == 0 {
            return;
        }
        // A token inherited from the desktop would take priority over the chosen CLI login.
        let overrides: &[&str] = match self.own_config {
            Some(Provider::Anthropic) => &[
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
                "CLAUDE_CODE_OAUTH_TOKEN",
                "CLAUDE_CODE_USE_BEDROCK",
                "CLAUDE_CODE_USE_VERTEX",
                "CLAUDE_CODE_USE_FOUNDRY",
            ],
            Some(Provider::Codex) => &["OPENAI_API_KEY", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"],
            Some(Provider::KimiCode) => &["KIMI_API_KEY", "KIMI_MODEL_API_KEY"],
            Some(Provider::Antigravity) => &["GEMINI_API_KEY", "GOOGLE_API_KEY", "AGY_ADC_AUTH"],
            _ => &[],
        };
        for variable in overrides {
            command.env_remove(variable);
        }
    }
}

pub type AgentProcess = Box<dyn ChildWrapper>;

/// Starts the agent in `worktree` with piped stdio for ACP. It loads your user and project
/// config as it would in a terminal: no isolated home, no empty config, no sandbox.
pub fn spawn_agent(launch: &AgentLaunch, worktree: &Path) -> Result<AgentProcess, ClientError> {
    launch.prepare_account()?;
    let program = find_program(launch.program.as_os_str());
    let mut command = hidden_command(&program);
    launch.apply_environment(&mut command);
    crate::providers::configure_codex_startup(&mut command, launch, &program);
    command
        .args(&launch.args)
        .args(launch.profile_args())
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    spawn_owned_command(command).map_err(|source| ClientError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })
}

/// Ownership groups terminate descendants even after their shell shim exits.
/// No resource or security limits are applied to the agent or its tools. The `CreationFlags`
/// wrap below replaces the flags `hidden_command` set, so it repeats `CREATE_NO_WINDOW`; clippy's
/// `disallowed_types` keeps every other process from using it (#276).
pub fn spawn_owned_command(mut command: Command) -> std::io::Result<AgentProcess> {
    extend_search_path(&mut command);
    wrap_owned_command(command)
}

/// Like [`spawn_owned_command`], but keeps the caller's `PATH` without prepending
/// Velocity's agent shims. Used by Velocity-owned checks (`preflight`) that invoke real
/// toolchain binaries (`cargo clippy`, `rustfmt`, `export-bindings`) directly.
pub(super) fn spawn_owned_toolchain_command(command: Command) -> std::io::Result<AgentProcess> {
    wrap_owned_command(command)
}

#[allow(clippy::disallowed_types)]
fn wrap_owned_command(command: Command) -> std::io::Result<AgentProcess> {
    let mut command = CommandWrap::from(command);
    command.wrap(KillOnDrop);
    #[cfg(windows)]
    {
        use process_wrap::tokio::{CreationFlags, JobObject};
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;
        command
            .wrap(CreationFlags(CREATE_NO_WINDOW))
            .wrap(JobObject);
    }
    #[cfg(unix)]
    command.wrap(process_wrap::tokio::ProcessGroup::leader());
    command.spawn()
}

pub async fn stop_owned_process(agent: &mut AgentProcess) -> std::io::Result<ExitStatus> {
    #[cfg(not(unix))]
    agent.start_kill()?;
    #[cfg(unix)]
    if let Err(error) = agent.start_kill() {
        // A Unix group that already exited has no remaining processes to signal.
        if error.raw_os_error() == Some(libc::ESRCH) {
            return agent.wait().await;
        }
        return Err(error);
    }
    agent.wait().await
}

pub async fn stop_agent(agent: &mut AgentProcess) -> std::io::Result<()> {
    stop_owned_process(agent).await.map(|_| ())
}

/// Gives the process the folders installed since Velocity started (`search_path`), so an
/// agent installed meanwhile finds its own runtime, such as a new `node`. A command that sets
/// its own `PATH` keeps it, with Rosen's folder first so both the provider's shell and
/// ACP terminals can run the machine-wide queue command (#308).
fn extend_search_path(command: &mut Command) {
    let own_path = command
        .as_std()
        .get_envs()
        .find(|(name, _)| name.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.map(OsStr::to_owned));
    let path = match own_path {
        Some(Some(existing)) => super::run_queue::search_path(Some(existing)),
        Some(None) => None,
        None => search_path(),
    };
    let Some(path) = path else {
        return;
    };
    command.env("PATH", path);
}

/// A `Command` that opens no console window. Velocity's release build is a windowed app, so on
/// Windows a child started without `CREATE_NO_WINDOW` gets a console window of its own that
/// flashes on screen (#232). Every agent, sign-in, install, limits probe and terminal command is
/// built here; clippy's `disallowed_methods` (`clippy.toml`) rejects a bare `Command::new`.
#[allow(clippy::disallowed_methods)]
pub fn hidden_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
