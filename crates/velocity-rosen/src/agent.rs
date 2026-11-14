//! What an agent working in a Rosen workspace is told and given.

use std::path::Path;

use crate::{ProjectServer, binary_folder};

/// The agent's Rosen name: its workspace folder's name, which Velocity keeps unique per
/// project and free of spaces, as Rosen requires.
pub fn agent_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().replace(char::is_whitespace, "-"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("velocity-agent"))
}

/// Environment for the agent's process, so the `rosen` CLI it runs reaches the project's
/// server as this agent: `ROSEN_ENDPOINT`, `ROSEN_AGENT`, and Rosen's folder first on
/// `PATH`.
pub fn agent_environment(server: &ProjectServer, agent: &str) -> Vec<(String, String)> {
    let mut environment = Vec::from([
        (
            String::from("ROSEN_ENDPOINT"),
            server.endpoint().as_str().to_owned(),
        ),
        (String::from("ROSEN_AGENT"), agent.to_owned()),
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
         `rosen claim --change {change} <paths>` claims files other agents might edit."
    )
}
