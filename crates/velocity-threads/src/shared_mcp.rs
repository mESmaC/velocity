//! Settings → Agents → shared MCP servers as ACP servers (SPEC.md §3.2 Tool access): one list
//! for every agent of every provider, set on each launch built from settings.

use agent_client_protocol::v1::schema::{McpServer, McpServerHttp, McpServerStdio};
use velocity_store::Settings;
use velocity_store::shared_mcp::SharedMcpTransport;

/// The shared servers in `settings`, in the order Settings → Agents lists them.
pub fn shared_mcp_servers(settings: &Settings) -> Vec<McpServer> {
    settings
        .agents
        .shared_mcp_servers
        .iter()
        .map(|server| match &server.transport {
            SharedMcpTransport::Command { program, arguments } => McpServer::Stdio(
                McpServerStdio::new(server.name.clone(), program).args(arguments.clone()),
            ),
            SharedMcpTransport::Url { url } => {
                McpServer::Http(McpServerHttp::new(server.name.clone(), url))
            }
        })
        .collect()
}
