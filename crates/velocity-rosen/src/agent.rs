//! What an agent working in a Rosen workspace is told and given.

use std::path::{Path, PathBuf};

use crate::{ProjectServer, binary_folder};

/// The names of the environment variables that tell `rosen` which server, agent and
/// change it acts for.
const ROSEN_VARIABLES: &str = "ROSEN_";

/// The agent's Rosen name: its workspace folder's name, which Velocity keeps unique per
/// project and free of spaces, as Rosen requires.
pub fn agent_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().replace(char::is_whitespace, "-"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("velocity-agent"))
}

/// Environment for the agent's process, so the `rosen` CLI it runs reaches the project's
/// server as this agent, on its change: `ROSEN_ENDPOINT`, `ROSEN_AGENT`, `ROSEN_CHANGE`,
/// and Rosen's folder first on `PATH`. `claim`, `share` and `release` take the change only
/// from `ROSEN_CHANGE` or `--change`, never from the folder they run in.
pub fn agent_environment(
    server: &ProjectServer,
    agent: &str,
    change: &str,
) -> Vec<(String, String)> {
    let mut environment = Vec::from([
        (
            String::from("ROSEN_ENDPOINT"),
            server.endpoint().as_str().to_owned(),
        ),
        (String::from("ROSEN_AGENT"), agent.to_owned()),
        (String::from("ROSEN_CHANGE"), change.to_owned()),
        // Velocity starts the project's server with its own data folder. A `rosen`
        // command that found it down would start one in the agent's folder instead, so it
        // is pointed at a program that doesn't exist and fails with this name.
        (
            String::from("ROSEN_SERVER"),
            String::from("velocity-starts-the-rosen-server"),
        ),
    ]);
    if let Ok(folder) = binary_folder() {
        let existing = std::env::var_os("PATH").unwrap_or_default();
        let folders = std::iter::once(folder).chain(std::env::split_paths(&existing));
        if let Ok(path) = std::env::join_paths(folders) {
            environment.push((String::from("PATH"), path.to_string_lossy().into_owned()));
        }
    }
    environment
}

/// Rosen's own MCP server for an agent (Rosen R10 `rosen mcp`): its commands as tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCommand {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    /// The `ROSEN_*` variables of the agent's environment, which the commands read.
    pub environment: Vec<(String, String)>,
}

/// The `rosen mcp` server for the agent working in workspace `root`, whose process has
/// `environment` ([`agent_environment`]). `None` when the `rosen` program isn't found: the
/// agent then has the CLI's failure to tell it so, not a server that can't start.
pub fn mcp_command(root: &Path, environment: &[(String, String)]) -> Option<McpCommand> {
    let program = binary_folder()
        .ok()?
        .join(format!("rosen{}", std::env::consts::EXE_SUFFIX));
    program.is_file().then(|| McpCommand {
        program,
        arguments: vec![
            String::from("mcp"),
            String::from("--workspace"),
            root.to_string_lossy().into_owned(),
        ],
        environment: environment
            .iter()
            .filter(|(name, _)| name.starts_with(ROSEN_VARIABLES))
            .cloned()
            .collect(),
    })
}

/// Sent with the agent's first message: its folder is a Rosen workspace, not a git
/// checkout, so the git and pull request steps in its instructions don't apply.
pub fn workspace_note(change: &str) -> String {
    format!(
        "[Velocity] Your working folder is a Rosen workspace for change `{change}`, not a git \
         checkout. Every file you write is stored on the Rosen server within 2 seconds, so \
         there is nothing to commit, push or open: skip any git branch, commit, push and pull \
         request steps your instructions give. When the work is done, name it with \
         `rosen commit -m \"<one-line summary>\"`; Velocity's PR duty reviews and lands the \
         change. `rosen status` shows what is stored, `rosen diff` your change, and \
         `rosen claim <paths>` claims files other agents might edit. The `rosen` MCP server \
         offers the same commands as tools (`status`, `diff`, `claim`, `overlaps`, `commit` \
         and the rest), returning JSON."
    )
}
