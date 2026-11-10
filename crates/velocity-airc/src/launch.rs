//! How an agent's AIRC MCP server is started (F14, §3.2): the command-line arguments Velocity
//! passes in `session/new`, and serving one connection from them on stdin/stdout.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{Channel, Error, serve};

/// The switch that makes Velocity's own executable serve AIRC, so the server ships inside
/// the app and needs no separate install.
pub const SERVER_SWITCH: &str = "--airc-mcp";

/// The MCP server name agents see: their tools are AIRC's `claim`, `read_feed` and so on.
pub const SERVER_NAME: &str = "airc";

const USAGE: &str = "Usage: <app-data> <project-id> <member-id> <worktree-leaf-or-duty-nick>";

/// The arguments that follow the program (or [`SERVER_SWITCH`]) for one agent's connection.
/// `member` names the agent's session for presence; `nick` is what the channel shows.
pub fn server_arguments(app_data: &Path, project: &str, member: &str, nick: &str) -> Vec<String> {
    vec![
        app_data.display().to_string(),
        project.to_owned(),
        member.to_owned(),
        nick.to_owned(),
    ]
}

/// Serves one agent's connection on stdin/stdout from [`server_arguments`]: joins on
/// `initialize`, leaves at end of input.
pub fn serve_stdio(arguments: &[OsString]) -> Result<(), Error> {
    let [app_data, project, member, nick] = arguments else {
        return Err(Error::Invalid(USAGE.into()));
    };
    let text = |value: &OsString, what: &str| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Invalid(format!("Invalid {what}")))
    };
    let channel = Channel::open(
        &PathBuf::from(app_data),
        &text(project, "project identity")?,
    )?;
    serve(
        &channel,
        &text(member, "member identity")?,
        &text(nick, "nick")?,
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )
}
