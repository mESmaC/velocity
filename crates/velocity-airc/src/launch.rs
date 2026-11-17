//! How an agent's AIRC MCP server is started (F14, §3.2): the command-line arguments Velocity
//! passes in `session/new`, and serving one connection from them on stdin/stdout.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::{Channel, Error, IssueUpdate, protocol::ToolOptions};

/// The switch that makes Velocity's own executable serve AIRC, so the server ships inside
/// the app and needs no separate install.
pub const SERVER_SWITCH: &str = "--airc-mcp";

/// The MCP server name agents see: their tools are AIRC's `claim`, `read_feed` and so on.
pub const SERVER_NAME: &str = "airc";

pub const CONTINUOUS_DISPATCH_ARGUMENT: &str = "--continuous-dispatch";

const USAGE: &str = "Usage: <app-data> <project-id> <member-id> <worktree-leaf-or-duty-nick> [member-scope] [--continuous-dispatch]";

/// The arguments that follow the program (or [`SERVER_SWITCH`]) for one agent's connection.
/// `member` names the agent's session for presence; `nick` is what the channel shows.
/// Capture the native runner's namespace before a provider starts the tool process.
pub fn server_arguments(app_data: &Path, project: &str, member: &str, nick: &str) -> Vec<String> {
    vec![
        app_data.display().to_string(),
        project.to_owned(),
        member.to_owned(),
        nick.to_owned(),
        crate::remote::member_scope(app_data),
    ]
}

/// Serves one agent's connection on stdin/stdout from [`server_arguments`]: joins on
/// `initialize`, leaves at end of input.
pub fn serve_stdio(arguments: &[OsString]) -> Result<(), Error> {
    serve_stdio_inner(arguments, None)
}

pub fn serve_stdio_with_issues(
    arguments: &[OsString],
    handler: impl Fn(&Path, &str, IssueUpdate<'_>) -> Result<String, Error>,
) -> Result<(), Error> {
    serve_stdio_inner(arguments, Some(&handler))
}

type StdioIssueHandler<'a> = dyn Fn(&Path, &str, IssueUpdate<'_>) -> Result<String, Error> + 'a;

fn serve_stdio_inner(
    arguments: &[OsString],
    handler: Option<&StdioIssueHandler<'_>>,
) -> Result<(), Error> {
    let [app_data, project, member, nick, options @ ..] = arguments else {
        return Err(Error::Invalid(USAGE.into()));
    };
    let (scope, continuous_dispatch) = match options {
        [scope @ .., marker] if marker == CONTINUOUS_DISPATCH_ARGUMENT => (scope, true),
        _ => (options, false),
    };
    if scope.len() > 1 {
        return Err(Error::Invalid(USAGE.into()));
    }
    let text = |value: &OsString, what: &str| {
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| Error::Invalid(format!("Invalid {what}")))
    };
    let app_data = PathBuf::from(app_data);
    let project = text(project, "project identity")?;
    let channel = match std::env::var("ROSEN_ENDPOINT") {
        Ok(endpoint) => Channel::at_endpoint(&app_data, &endpoint),
        Err(_) => Channel::open(&app_data, &project)?,
    }
    .with_member_scope(
        scope
            .first()
            .map(|scope| text(scope, "member scope"))
            .transpose()?,
    );
    let update = |request: IssueUpdate<'_>| {
        handler.ok_or_else(|| Error::Invalid("No GitHub issue transport".into()))?(
            &app_data, &project, request,
        )
    };
    crate::protocol::serve_with_options(
        &channel,
        &text(member, "member identity")?,
        &text(nick, "nick")?,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
        ToolOptions::new(
            continuous_dispatch,
            handler.map(|_| &update as &crate::issue_tools::IssueHandler<'_>),
        ),
    )
}
