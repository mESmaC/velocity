//! The project's AIRC channel as an MCP server in `session/new` (F14, §3.2): every agent
//! Velocity starts gets it, whatever its provider or role, with no per-provider setup.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use velocity_airc::{SERVER_NAME, SERVER_SWITCH, server_arguments};

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
        self.server(project, &format!("duty-{nick}"), nick)
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
