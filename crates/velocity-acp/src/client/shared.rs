//! Shared MCP servers that come from the app rather than from one launch path (§3.2 Tool
//! access): every `session/new`, whatever role starts it, asks one source for the servers
//! that belong to the session's folder. Skill packs with an MCP server (F18: REA) reach every
//! agent of every provider this way, with no per-agent or per-role setup.

use std::path::Path;
use std::sync::OnceLock;

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};

/// A stdio MCP server the app shares with every agent started in a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedServer {
    pub name: String,
    pub command: String,
    pub arguments: Vec<String>,
}

impl SharedServer {
    pub fn new(name: String, command: String, arguments: Vec<String>) -> Self {
        Self {
            name,
            command,
            arguments,
        }
    }

    fn into_server(self) -> McpServer {
        McpServer::Stdio(McpServerStdio::new(self.name, self.command).args(self.arguments))
    }
}

type Source = dyn Fn(&Path) -> Vec<SharedServer> + Send + Sync;

static SOURCE: OnceLock<Box<Source>> = OnceLock::new();

/// Installs the app's source once, at startup. A second call is ignored and returns false:
/// sessions already started must not see the list change source under them.
pub fn install_shared_servers(
    source: impl Fn(&Path) -> Vec<SharedServer> + Send + Sync + 'static,
) -> bool {
    SOURCE.set(Box::new(source)).is_ok()
}

/// The source's servers for a session working in `folder`; none before a source is installed
/// (examples and headless drivers).
pub(crate) fn shared_servers_for(folder: &Path) -> Vec<McpServer> {
    SOURCE
        .get()
        .map(|source| {
            source(folder)
                .into_iter()
                .map(SharedServer::into_server)
                .collect()
        })
        .unwrap_or_default()
}
