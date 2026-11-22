//! The project's AIRC channel as an MCP server in `session/new` (F14, §3.2): every agent
//! Velocity starts gets it, whatever its provider or role, with no per-provider setup.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{EnvVariable, McpServer, McpServerStdio};
use velocity_airc::{
    CONTINUOUS_DISPATCH_ARGUMENT, LAZY_ARGUMENT, ROLE_ARGUMENT, SERVER_NAME, SERVER_SWITCH,
    server_arguments,
};
use velocity_core::tool_policy::ThreadRole;

use super::session::SessionSetup;

/// Publish velocity-graph's shared guidance, workspace map (subsystem counts and popular
/// symbols) and task matches once per session in the preface, not in every tool description (#538).
pub(super) async fn graph_preface(setup: &SessionSetup) -> Option<String> {
    let (airc, app_data, project) = airc_project(setup.airc.as_ref()?)?;
    let mut note = String::from(velocity_graph::INSTRUCTIONS);
    let app_data = PathBuf::from(app_data);
    let executable = airc.command.clone();
    let project = project.to_owned();
    let worktree = setup.worktree.clone();
    let title: String = setup
        .graph_task_title
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .chars()
        .take(300)
        .collect();
    let lookup = tokio::task::spawn_blocking(move || {
        let map = velocity_graph::workspace_map(&executable, &app_data, &project, &worktree);
        let matches = if title.is_empty() || velocity_graph::is_conversational_prompt(&title) {
            None
        } else {
            velocity_graph::task_context(&executable, &app_data, &project, &worktree, &title)
        };
        (map, matches)
    });
    if let Ok(Ok((map, matches))) =
        tokio::time::timeout(std::time::Duration::from_secs(5), lookup).await
    {
        if let Some(map) = map {
            note.push_str("\n\n");
            note.push_str(&map);
        }
        if let Some(matches) = matches {
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
/// Maps unified thread agent names (`<slug>-<id>`) back to `thread-<id>` so code-graph
/// usage records stay keyed by thread identifier across #532.
pub(super) fn usage_thread(airc: &McpServerStdio) -> Option<String> {
    let member = airc.args.get(3)?.as_str();
    if member
        .strip_prefix("spec-")
        .is_some_and(|id| id.parse::<u32>().is_ok())
    {
        return Some(member.to_owned());
    }
    velocity_core::thread_titles::thread_id_from_member(member).map(|id| format!("thread-{id}"))
}

/// Applies `--role <role>` and optional `--lazy` to an AIRC stdio server made by [`AircLaunch`].
pub(super) fn with_airc_policy(server: &McpServer, role: ThreadRole, lazy: bool) -> McpServer {
    let McpServer::Stdio(stdio) = server else {
        return server.clone();
    };
    if stdio.args.first().map(String::as_str) != Some(SERVER_SWITCH) {
        return server.clone();
    }
    let mut updated = stdio.clone();
    if !updated.args.iter().any(|arg| arg == ROLE_ARGUMENT) {
        updated.args.push(ROLE_ARGUMENT.to_owned());
        updated.args.push(role.as_str().to_owned());
    }
    if lazy && !updated.args.iter().any(|arg| arg == LAZY_ARGUMENT) {
        updated.args.push(LAZY_ARGUMENT.to_owned());
    }
    McpServer::Stdio(updated)
}

/// The code graph server (F19) for a session that has AIRC: the same executable and project,
/// over the session's worktree. Every session Velocity starts gets AIRC, so deriving the graph
/// from it means no role can start without the graph. `None` for a server [`AircLaunch`]
/// didn't make.
/// While the project's code graph switch is off (F19), sessions start without it.
/// `role` and `lazy` pick the thread role's tool set (#533).
pub(super) fn graph_server_for_role(
    airc: &McpServer,
    worktree: &Path,
    role: Option<ThreadRole>,
    lazy: bool,
) -> Option<McpServer> {
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
    arguments.extend(usage_thread(airc));
    if let Some(role) = role {
        arguments.push(velocity_graph::ROLE_FLAG.to_owned());
        arguments.push(role.as_str().to_owned());
        if lazy {
            arguments.push(velocity_graph::LAZY_FLAG.to_owned());
        }
    }
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
    gh_program: Option<PathBuf>,
}

impl AircLaunch {
    pub fn new(executable: PathBuf, app_data: PathBuf) -> Self {
        Self {
            executable,
            app_data,
            gh_program: velocity_github::app_gh_program().map(Path::to_path_buf),
        }
    }

    /// Overrides the `gh` executable passed to the AIRC MCP server via `VELOCITY_GH` (#581).
    pub fn with_gh_program(mut self, gh_program: Option<PathBuf>) -> Self {
        self.gh_program = gh_program;
        self
    }

    /// A build thread's connection using its unified unique agent name (#532).
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
        let leaf = worktree
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .filter(|leaf| !leaf.trim().is_empty())
            .unwrap_or("thread");
        let name = velocity_core::thread_titles::thread_agent_name(leaf, thread);
        self.thread_member(project, &name, &name, continuous_dispatch)
    }

    /// A build thread's connection with explicit member and nick (unified name for new
    /// threads, preserving `thread-<id>` member for pre-#532 saved threads).
    pub fn thread_member(
        &self,
        project: u32,
        member: &str,
        nick: &str,
        continuous_dispatch: bool,
    ) -> McpServer {
        let mut server = self.stdio(project, member, nick);
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
        let mut server = McpServerStdio::new(SERVER_NAME, &self.executable).args(arguments);
        if let Some(gh_program) = &self.gh_program {
            server = server.env(vec![EnvVariable::new(
                velocity_github::GH_PROGRAM_ENV,
                gh_program.to_string_lossy().into_owned(),
            )]);
        }
        server
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn airc_launch_passes_resolved_gh_program_through_policy() {
        // Risk (#581): the AIRC MCP server must receive VELOCITY_GH from AircLaunch so it
        // runs the real gh binary even when the agent's PATH puts Velocity's shim first.
        let gh_path = PathBuf::from("/opt/github-cli/gh");
        let launch = AircLaunch::new(PathBuf::from("/app/velocity"), PathBuf::from("/app/data"))
            .with_gh_program(Some(gh_path.clone()));
        let server = launch.duty(1, "scout-1");
        let updated = with_airc_policy(&server, ThreadRole::Builder, true);
        let McpServer::Stdio(stdio) = updated else {
            panic!("AIRC server should stay stdio");
        };
        assert!(stdio.env.iter().any(|var| {
            var.name == velocity_github::GH_PROGRAM_ENV && var.value == gh_path.to_string_lossy()
        }));
    }

    // Risk (#678): build threads with `spec-*` or disambiguated `duty-*` names must retain
    // per-thread graph and ACP token usage attribution while actual spec interviews and duty
    // agents keep their existing attribution.
    #[test]
    fn usage_attribution_handles_reserved_slug_build_threads() {
        use velocity_graph::token_usage::{Role, Subject};

        let launch = AircLaunch::new(PathBuf::from("/app/velocity"), PathBuf::from("/app/data"));
        let check = |server: McpServer, expected_graph: Option<&str>, expected_subject: Subject| {
            let McpServer::Stdio(stdio) = &server else {
                panic!("AIRC server should stay stdio");
            };
            assert_eq!(usage_thread(stdio).as_deref(), expected_graph);
            let mut setup = SessionSetup::new(PathBuf::from("/app/work"), Vec::new());
            setup.airc = Some(server);
            let ledger = super::super::token_usage::Ledger::for_setup(&setup);
            assert_eq!(ledger.map(|item| item.subject), Some(expected_subject));
        };

        check(
            launch.thread(1, 50, Path::new(r"Z:\work\spec-cleanup")),
            Some("thread-50"),
            Subject {
                role: Role::Implementation,
                thread: Some(50),
            },
        );
        check(
            launch.thread(1, 50, Path::new(r"Z:\work\spec")),
            Some("thread-50"),
            Subject {
                role: Role::Implementation,
                thread: Some(50),
            },
        );
        check(
            launch.thread(1, 50, Path::new(r"Z:\work\duty-cleanup")),
            Some("thread-50"),
            Subject {
                role: Role::Implementation,
                thread: Some(50),
            },
        );
        check(
            launch.spec_interview(1, 12),
            Some("spec-12"),
            Subject {
                role: Role::Implementation,
                thread: Some(12),
            },
        );
        check(
            launch.duty(1, "scout-1"),
            None,
            Subject {
                role: Role::Duty,
                thread: None,
            },
        );
    }
}
