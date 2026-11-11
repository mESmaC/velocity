//! Settings → Agents → shared MCP servers (SPEC.md §3.2 Tool access, F13): the servers passed
//! to every agent of every provider in `session/new`, so one list reaches all four providers.
//!
//! A server is a name and either a command Velocity's agents start or a URL. There is no
//! environment or header field: a server that needs a credential reads it from your own
//! environment or signs in itself, so `state.json` never holds a key (SPEC.md §1, F15).

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::providers::AgentCommand;
use crate::{StateStore, StoreError};

/// The longest name the agents' configs all accept as an MCP server key.
const MAX_NAME_LENGTH: usize = 64;

/// Names of Velocity's own servers (`velocity-airc`, `velocity-pr-duty`, …) start with this.
/// A shared server with such a name would be dropped behind Velocity's, so it is refused.
const RESERVED_PREFIX: &str = "velocity";

/// One shared MCP server, in the order Settings → Agents lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SharedMcpServer {
    pub name: String,
    pub transport: SharedMcpTransport,
}

/// How an agent reaches a shared server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SharedMcpTransport {
    /// A stdio server the agent starts, with your `PATH` and environment (§3.2 Tool access).
    Command {
        program: String,
        arguments: Vec<String>,
    },
    /// A streamable HTTP server. Agents that don't speak MCP over HTTP aren't sent it.
    Url { url: String },
}

/// A change from Settings → Agents.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum SharedMcpServerEdit {
    /// `target` is typed in one field: an `http://` or `https://` URL, or a command line read
    /// like a provider's command (double quotes around a word with spaces).
    Add {
        name: String,
        target: String,
    },
    Remove {
        name: String,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum SharedMcpServerError {
    #[error("Name the server")]
    MissingName,
    #[error("Server names use letters, digits, - and _ (at most 64): `{0}`")]
    InvalidName(String),
    #[error("Names starting with `velocity` belong to Velocity's own servers: `{0}`")]
    ReservedName(String),
    #[error("A shared server named {0} already exists")]
    NameTaken(String),
    #[error("Enter the server's command or URL")]
    MissingTarget,
    #[error("The URL has no host: `{0}`")]
    InvalidUrl(String),
    #[error("No shared server is named {0}")]
    UnknownServer(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl SharedMcpServer {
    /// The server `target` describes: a URL when it starts with `http://` or `https://`,
    /// otherwise a command line.
    pub fn parse(name: &str, target: &str) -> Result<Self, SharedMcpServerError> {
        let name = name.trim();
        check_name(name)?;
        let target = target.trim();
        if target.is_empty() {
            return Err(SharedMcpServerError::MissingTarget);
        }
        let transport = match url_host(target) {
            Some("") => {
                return Err(SharedMcpServerError::InvalidUrl(target.into()));
            }
            Some(_) => SharedMcpTransport::Url { url: target.into() },
            None => {
                let command =
                    AgentCommand::parse(target).ok_or(SharedMcpServerError::MissingTarget)?;
                SharedMcpTransport::Command {
                    program: command.program,
                    arguments: command.arguments,
                }
            }
        };
        Ok(Self {
            name: name.into(),
            transport,
        })
    }
}

/// The part after `http://` or `https://` up to the path, or `None` when `target` is not a
/// URL. Whitespace in a URL counts as no host, since agents would reject it.
fn url_host(target: &str) -> Option<&str> {
    let lower = target.to_ascii_lowercase();
    let scheme_length = ["https://", "http://"]
        .into_iter()
        .find(|scheme| lower.starts_with(scheme))?
        .len();
    let rest = &target[scheme_length..];
    if rest.contains(char::is_whitespace) {
        return Some("");
    }
    Some(rest.split(['/', '?', '#']).next().unwrap_or_default())
}

fn check_name(name: &str) -> Result<(), SharedMcpServerError> {
    if name.is_empty() {
        return Err(SharedMcpServerError::MissingName);
    }
    let allowed = |character: char| character.is_ascii_alphanumeric() || "-_".contains(character);
    if name.len() > MAX_NAME_LENGTH || !name.chars().all(allowed) {
        return Err(SharedMcpServerError::InvalidName(name.into()));
    }
    if name.to_ascii_lowercase().starts_with(RESERVED_PREFIX) {
        return Err(SharedMcpServerError::ReservedName(name.into()));
    }
    Ok(())
}

/// Every name valid and unique (ignoring case, since agents' configs treat keys loosely on
/// Windows), and every server with a target. `set_settings` checks a whole list with this.
pub fn validate_shared_mcp_servers(
    servers: &[SharedMcpServer],
) -> Result<(), SharedMcpServerError> {
    for (index, server) in servers.iter().enumerate() {
        check_name(&server.name)?;
        let empty = match &server.transport {
            SharedMcpTransport::Command { program, .. } => program.trim().is_empty(),
            SharedMcpTransport::Url { url } => url_host(url).is_none_or(str::is_empty),
        };
        if empty {
            return Err(SharedMcpServerError::MissingTarget);
        }
        if servers[..index]
            .iter()
            .any(|earlier| earlier.name.eq_ignore_ascii_case(&server.name))
        {
            return Err(SharedMcpServerError::NameTaken(server.name.clone()));
        }
    }
    Ok(())
}

impl StateStore {
    /// Adds or removes a shared server and saves. Returns the list as saved.
    pub fn edit_shared_mcp_servers(
        &mut self,
        edit: SharedMcpServerEdit,
    ) -> Result<Vec<SharedMcpServer>, SharedMcpServerError> {
        let mut settings = self.settings();
        let servers = &mut settings.agents.shared_mcp_servers;
        match edit {
            SharedMcpServerEdit::Add { name, target } => {
                let server = SharedMcpServer::parse(&name, &target)?;
                if servers
                    .iter()
                    .any(|existing| existing.name.eq_ignore_ascii_case(&server.name))
                {
                    return Err(SharedMcpServerError::NameTaken(server.name));
                }
                servers.push(server);
            }
            SharedMcpServerEdit::Remove { name } => {
                let before = servers.len();
                servers.retain(|server| server.name != name);
                if servers.len() == before {
                    return Err(SharedMcpServerError::UnknownServer(name));
                }
            }
        }
        Ok(self.set_settings(settings)?.agents.shared_mcp_servers)
    }
}
