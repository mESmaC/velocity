//! Launching an agent: one process per thread, in that thread's worktree (§3.2).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

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

/// Starts the agent in `worktree` with piped stdio for ACP. It loads your user and project
/// config as it would in a terminal: no isolated home, no empty config, no sandbox.
pub fn spawn_agent(launch: &AgentLaunch, worktree: &Path) -> Result<Child, ClientError> {
    let mut command = Command::new(find_program(launch.program.as_os_str()));
    command
        .args(&launch.args)
        .envs(launch.env.iter().map(|(name, value)| (name, value)))
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console_window(&mut command);
    command.spawn().map_err(|source| ClientError::Spawn {
        program: launch.program.display().to_string(),
        source,
    })
}

/// Stops the agent and every process it started. Agents installed with npm run through a
/// `.cmd` shim, so the child is `cmd.exe` and the agent is its `node` grandchild: killing only
/// the child leaves the agent running with the pipes open.
pub async fn kill_agent_tree(agent: &mut Child) {
    #[cfg(windows)]
    if let Some(process_id) = agent.id() {
        let mut taskkill = Command::new("taskkill");
        taskkill
            .args(["/T", "/F", "/PID", &process_id.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console_window(&mut taskkill);
        // Already exited, or taskkill missing: `start_kill` below still stops the child.
        let _ = taskkill.status().await;
    }
    let _ = agent.start_kill();
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
