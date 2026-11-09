//! Launching an agent: one process per thread, in that thread's worktree (§3.2).

use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::process::Command;

use super::ClientError;
use super::program_path::find_program;

/// The command that runs one provider's ACP agent. This is the only provider-specific part
/// of a session (§3.2 Provider code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentLaunch {
    /// The agent's executable: a path, or a bare name looked up on `PATH`, including `.cmd`
    /// shims such as `claude-code-acp` on Windows (`program_path::find_program`).
    pub program: PathBuf,
    pub args: Vec<String>,
    /// Added to Velocity's own environment, which the agent inherits whole: your `PATH`,
    /// toolchains, credentials and config (§3.2 Tool access).
    pub env: Vec<(String, String)>,
}

impl AgentLaunch {
    pub fn new(program: PathBuf) -> Self {
        Self {
            program,
            args: Vec::new(),
            env: Vec::new(),
        }
    }
}

pub type AgentProcess = Box<dyn ChildWrapper>;

/// Starts the agent in `worktree` with piped stdio for ACP. It loads your user and project
/// config as it would in a terminal: no isolated home, no empty config, no sandbox.
pub fn spawn_agent(launch: &AgentLaunch, worktree: &Path) -> Result<AgentProcess, ClientError> {
    let mut command = Command::new(find_program(launch.program.as_os_str()));
    command
        .args(&launch.args)
        .envs(launch.env.iter().map(|(name, value)| (name, value)))
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
/// No resource or security limits are applied to the agent or its tools.
pub fn spawn_owned_command(mut command: Command) -> std::io::Result<AgentProcess> {
    hide_console_window(&mut command);
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
    agent.start_kill()?;
    agent.wait().await
}

pub async fn stop_agent(agent: &mut AgentProcess) -> std::io::Result<()> {
    stop_owned_process(agent).await.map(|_| ())
}

/// Velocity is a windowed app: without this flag every agent and terminal command would
/// open its own console window.
#[cfg(windows)]
pub fn hide_console_window(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console_window(_command: &mut Command) {}
