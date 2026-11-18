//! The project's AIRC channel as an MCP server in `session/new` (F14, §3.2): every agent
//! Velocity starts gets it, whatever its provider or role, with no per-provider setup.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use velocity_airc::{CONTINUOUS_DISPATCH_ARGUMENT, SERVER_NAME, SERVER_SWITCH, server_arguments};

use super::session::SessionSetup;

/// Keep the initial hint even if indexing cannot return matches within the startup budget.
pub(super) async fn graph_preface(setup: &SessionSetup) -> Option<String> {
    let title = setup.graph_task_title.as_ref()?.trim();
    let (airc, app_data, project) = airc_project(setup.airc.as_ref()?)?;
    let mut note = String::from(
        "Use the workspace's velocity-graph tools first for source navigation. \
         For each code lookup, use graph_read on a match below or on a known file path, \
         or use graph_find with a short name to locate the symbol. \
         To inspect code from the file outline, pass the symbol's address to graph_read with \
         source: true. Use graph_impact or graph_run to trace connections between files. \
         For gaps in the graph, keep native reads to the relevant line ranges. \
         Use grep for exact literals and config values.",
    );
    if !title.is_empty() {
        let app_data = PathBuf::from(app_data);
        let executable = airc.command.clone();
        let project = project.to_owned();
        let worktree = setup.worktree.clone();
        let title: String = title.chars().take(300).collect();
        let lookup = tokio::task::spawn_blocking(move || {
            velocity_graph::task_context(&executable, &app_data, &project, &worktree, &title)
        });
        if let Ok(Ok(Some(matches))) =
            tokio::time::timeout(std::time::Duration::from_secs(5), lookup).await
        {
            note.push_str("\n\nTop graph_find matches for the task title:\n");
            note.push_str(&matches);
        }
    }
    Some(note)
}

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

/// Saved threads keep their identity across sessions and workspace renames.
pub(super) fn usage_thread(airc: &McpServerStdio) -> Option<&str> {
    let member = airc.args.get(3)?.as_str();
    ["thread-", "spec-"]
        .iter()
        .any(|prefix| {
            member
                .strip_prefix(prefix)
                .is_some_and(|id| id.parse::<u32>().is_ok())
        })
        .then_some(member)
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
    arguments.extend(usage_thread(airc).map(str::to_owned));
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
        self.thread_with_origin(project, thread, worktree, false)
    }

    pub fn thread_with_origin(
        &self,
        project: u32,
        thread: u32,
        worktree: &Path,
        continuous_dispatch: bool,
    ) -> McpServer {
        let member = format!("thread-{thread}");
        let nick = worktree
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .filter(|leaf| !leaf.trim().is_empty())
            .unwrap_or(&member)
            .to_owned();
        let mut server = self.stdio(project, &member, &nick);
        if continuous_dispatch {
            server.args.push(CONTINUOUS_DISPATCH_ARGUMENT.to_owned());
        }
        McpServer::Stdio(server)
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
        McpServer::Stdio(self.stdio(project, member, nick))
    }

    fn stdio(&self, project: u32, member: &str, nick: &str) -> McpServerStdio {
        let mut arguments = vec![SERVER_SWITCH.to_owned()];
        arguments.extend(server_arguments(
            &self.app_data,
            &project.to_string(),
            member,
            nick,
        ));
        McpServerStdio::new(SERVER_NAME, &self.executable).args(arguments)
    }
}
