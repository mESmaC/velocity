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

/// Ends the agent and every process it started. On Windows an npm agent runs as
/// `cmd.exe → node.exe`, and killing the shim alone orphans `node.exe`, which keeps running
/// with the worktree as its working directory, so the worktree cannot be deleted (#47, F12).
/// `taskkill /T` ends the tree while the shim still links it together; then the shim goes.
pub async fn stop_agent(agent: &mut Child) {
    #[cfg(windows)]
    if let Some(process_id) = agent.id() {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let mut command = Command::new(
            Path::new(&system_root)
                .join("System32")
                .join("taskkill.exe"),
        );
        command
            .args(["/PID", &process_id.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        hide_console_window(&mut command);
        // An agent that already exited has no tree left to end.
        let _ = command.status().await;
    }
    let _ = agent.start_kill();
    // Reaped, so its handles on the worktree are released before the session reports its end.
    let _ = agent.wait().await;
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
