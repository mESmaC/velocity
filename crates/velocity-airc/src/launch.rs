//! How an agent's AIRC MCP server is started (F14, §3.2): the command-line arguments Velocity
//! passes in `session/new`, and serving one connection from them on stdin/stdout.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{Channel, Error, IssueRequest, NextWorkRequest, protocol::ToolOptions};

/// The switch that makes Velocity's own executable serve AIRC, so the server ships inside
/// the app and needs no separate install.
pub const SERVER_SWITCH: &str = "--airc-mcp";

/// The MCP server name agents see: their tools are AIRC's `claim`, `read_feed` and so on.
pub const SERVER_NAME: &str = "airc";

pub const CONTINUOUS_DISPATCH_ARGUMENT: &str = "--continuous-dispatch";
pub const ROLE_ARGUMENT: &str = "--role";
pub const LAZY_ARGUMENT: &str = "--lazy";

const USAGE: &str = "Usage: <app-data> <project-id> <member-id> <worktree-leaf-or-duty-nick> [member-scope] [--continuous-dispatch] [--role <role>] [--lazy]";

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
    serve_stdio_inner(arguments, None, None)
}

pub fn serve_stdio_with_issues(
    arguments: &[OsString],
    handler: impl Fn(&Path, &str, IssueRequest<'_>) -> Result<Value, Error>,
) -> Result<(), Error> {
    serve_stdio_inner(arguments, Some(&handler), None)
}

pub fn serve_stdio_with_handlers(
    arguments: &[OsString],
    issue_handler: impl Fn(&Path, &str, IssueRequest<'_>) -> Result<Value, Error>,
    next_work_handler: impl Fn(&Path, &str, &Channel, NextWorkRequest<'_>) -> Result<Value, Error>,
) -> Result<(), Error> {
    serve_stdio_inner(arguments, Some(&issue_handler), Some(&next_work_handler))
}

type StdioIssueHandler<'a> = dyn Fn(&Path, &str, IssueRequest<'_>) -> Result<Value, Error> + 'a;
type StdioNextWorkHandler<'a> =
    dyn Fn(&Path, &str, &Channel, NextWorkRequest<'_>) -> Result<Value, Error> + 'a;

fn serve_stdio_inner(
    arguments: &[OsString],
    handler: Option<&StdioIssueHandler<'_>>,
    next_work: Option<&StdioNextWorkHandler<'_>>,
) -> Result<(), Error> {
    let [app_data, project, member, nick, options @ ..] = arguments else {
        return Err(Error::Invalid(USAGE.into()));
    };
    let (scope, continuous_dispatch, role, lazy) = parse_options(options)?;
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
    .with_member_scope(scope.map(|scope| text(scope, "member scope")).transpose()?)?;
    let handle_issue = |request: IssueRequest<'_>| {
        handler.ok_or_else(|| Error::Invalid("No GitHub issue transport".into()))?(
            &app_data, &project, request,
        )
    };
    let handle_next_work = |channel: &Channel, request: NextWorkRequest<'_>| {
        next_work.ok_or_else(|| Error::Invalid("next_work is unavailable".into()))?(
            &app_data, &project, channel, request,
        )
    };
    let mut options = ToolOptions::new(
        continuous_dispatch,
        handler.map(|_| &handle_issue as &crate::issue_tools::IssueHandler<'_>),
        Some(&app_data),
    );
    if let Some(role) = role {
        options = options.with_role(role, lazy);
    }
    if next_work.is_some() {
        options = options
            .with_next_work(&handle_next_work as &crate::next_work_tools::NextWorkHandler<'_>);
    }
    crate::protocol::serve_with_options(
        &channel,
        &text(member, "member identity")?,
        &text(nick, "nick")?,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
        options,
    )
}

/// Parsed launch options: scope, continuous dispatch, thread role and lazy tools.
type LaunchOptions<'a> = (
    Option<&'a OsString>,
    bool,
    Option<velocity_core::tool_policy::ThreadRole>,
    bool,
);

fn parse_options(options: &[OsString]) -> Result<LaunchOptions<'_>, Error> {
    let mut scope = None;
    let mut continuous_dispatch = false;
    let mut role = None;
    let mut lazy = false;
    let mut index = 0;
    while let Some(option) = options.get(index) {
        match option.to_str() {
            Some(CONTINUOUS_DISPATCH_ARGUMENT) => {
                continuous_dispatch = true;
                index += 1;
            }
            Some(LAZY_ARGUMENT) => {
                lazy = true;
                index += 1;
            }
            Some(ROLE_ARGUMENT) => {
                let value = options
                    .get(index + 1)
                    .and_then(|value| value.to_str())
                    .ok_or_else(|| Error::Invalid(USAGE.into()))?;
                role = Some(
                    velocity_core::tool_policy::ThreadRole::parse(value)
                        .ok_or_else(|| Error::Invalid(format!("Unknown thread role: {value}")))?,
                );
                index += 2;
            }
            _ if scope.is_none() => {
                scope = Some(option);
                index += 1;
            }
            _ => return Err(Error::Invalid(USAGE.into())),
        }
    }
    Ok((scope, continuous_dispatch, role, lazy))
}
