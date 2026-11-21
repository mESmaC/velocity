//! What an agent working in a Rosen workspace is told and given.

use std::path::{Path, PathBuf};

use crate::ProjectServer;
use crate::server::program_folder;

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
/// and the managed command folder first on `PATH`. `claim`, `share` and `release` take the change only
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
    let existing = std::env::var_os("PATH").unwrap_or_default();
    if let Some(path) = path_with_rosen(&existing.to_string_lossy()) {
        environment.push((String::from("PATH"), path));
    }
    environment
}

/// `existing` (a `PATH` value) with the managed commands and Rosen before other programs.
pub fn path_with_rosen(existing: &str) -> Option<String> {
    let managed = crate::agent_run::command_folder().map(Path::to_owned);
    let rosen = program_folder("rosen");
    if managed.is_none() && rosen.is_none() {
        return None;
    }
    let folders = managed.into_iter().chain(rosen).chain(
        std::env::split_paths(existing)
            .filter(|folder| !crate::agent_run::is_command_folder(folder)),
    );
    let path = std::env::join_paths(folders).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// Fallback note for non-Rosen sessions where `velocity-run` is unavailable (#534).
pub fn run_queue_note() -> &'static str {
    RUN_QUEUE_NOTE
}

const RUN_QUEUE_NOTE: &str = "[Velocity] `velocity-run` is unavailable in this session; \
     fall back to `rosen run -- <command>` for checks and builds. See AGENTS.md for rules.";

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
    let program = program_folder("rosen")?.join(format!("rosen{}", std::env::consts::EXE_SUFFIX));
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

/// Short Rosen workspace header for callers that do not assemble a full session card (#534).
/// Static workflow and build rules live in AGENTS.md so they are never duplicated or contradictory.
pub fn workspace_note(change: &str) -> String {
    format!("[Velocity] Rosen workspace · change `{change}` (rules live in AGENTS.md).")
}
