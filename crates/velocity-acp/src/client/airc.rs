//! The project's AIRC channel as an MCP server in `session/new` (F14, §3.2): every agent
//! Velocity starts gets it, whatever its provider or role, with no per-provider setup.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use velocity_airc::{SERVER_NAME, SERVER_SWITCH, server_arguments};

/// The app data folder and project an AIRC server [`AircLaunch`] made is for.
pub(super) fn airc_project(airc: &McpServer) -> Option<(&McpServerStdio, &str, &str)> {
    let McpServer::Stdio(airc) = airc else {
        return None;
    };
    // `AircLaunch::server` lays the arguments out as switch, app data, project, member, nick.
    let [switch, app_data, project, ..] = airc.args.as_slice() else {
        return None;
    };
    (switch == SERVER_SWITCH).then_some((airc, app_data.as_str(), project.as_str()))
}

/// The code graph server (F19) for a session that has AIRC: the same executable and project,
/// over the session's worktree. Every session Velocity starts gets AIRC, so deriving the graph
/// from it means no role can start without the graph. `None` for a server [`AircLaunch`]
/// didn't make.
/// While the project's code graph switch is off (F19), sessions start without it.
pub fn graph_server(airc: &McpServer, worktree: &Path) -> Option<McpServer> {
    let (airc, app_data, project) = airc_project(airc)?;
    if !velocity_graph::usage::enabled(Path::new(app_data), project) {
        return None;
    }
    let mut arguments = vec![velocity_graph::SERVER_SWITCH.to_owned()];
    arguments.extend(velocity_graph::server_arguments(
        Path::new(app_data),
        project,
        worktree,
    ));
    Some(McpServer::Stdio(
        McpServerStdio::new(velocity_graph::SERVER_NAME, &airc.command).args(arguments),
    ))
}

/// The AIRC member a duty agent's sessions join as, so delivery can find its nick (F14).
pub fn duty_member(nick: &str) -> String {
    format!("duty-{nick}")
}

/// Where the AIRC server comes from: Velocity's own executable started with
/// [`SERVER_SWITCH`], over the channels kept under the app data folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AircLaunch {
    executable: PathBuf,
    app_data: PathBuf,
}

impl AircLaunch {
    pub fn new(executable: PathBuf, app_data: PathBuf) -> Self {
        Self {
            executable,
            app_data,
        }
    }

    /// A build thread's connection. Its nick is the worktree leaf (F14 Nicks).
    pub fn thread(&self, project: u32, thread: u32, worktree: &Path) -> McpServer {
        let member = format!("thread-{thread}");
        let nick = worktree
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .filter(|leaf| !leaf.trim().is_empty())
            .unwrap_or(&member)
            .to_owned();
        self.server(project, &member, &nick)
    }

    /// A duty agent's connection under its role nick (`scout-1`, `pr-1`).
    pub fn duty(&self, project: u32, nick: &str) -> McpServer {
        self.server(project, &duty_member(nick), nick)
    }

    /// The spec interview's connection. It works in the main checkout, so it has no
    /// worktree leaf to be named after (F3).
    pub fn spec_interview(&self, project: u32, thread: u32) -> McpServer {
        self.server(project, &format!("spec-{thread}"), "spec")
    }

    fn server(&self, project: u32, member: &str, nick: &str) -> McpServer {
        let mut arguments = vec![SERVER_SWITCH.to_owned()];
        arguments.extend(server_arguments(
            &self.app_data,
            &project.to_string(),
            member,
            nick,
        ));
        McpServer::Stdio(McpServerStdio::new(SERVER_NAME, &self.executable).args(arguments))
    }
}
