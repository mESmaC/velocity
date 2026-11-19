//! Settings → Agents → shared MCP servers as ACP servers (SPEC.md §3.2 Tool access): one list
//! for every agent of every provider, set on each launch built from settings.

use agent_client_protocol::v1::schema::{HttpHeader, McpServer, McpServerHttp, McpServerStdio};
use velocity_store::Settings;
use velocity_store::shared_mcp::SharedMcpTransport;

/// The shared servers in `settings`, in the order Settings → Agents lists them, then
/// Collapse's read-only server when Collapse is connected and switched on.
pub fn shared_mcp_servers(settings: &Settings) -> Vec<McpServer> {
    let servers = &settings.agents.shared_mcp_servers;
    let mut listed: Vec<McpServer> = servers
        .iter()
        .map(|server| match &server.transport {
            SharedMcpTransport::Command { program, arguments } => McpServer::Stdio(
                McpServerStdio::new(server.name.clone(), program).args(arguments.clone()),
            ),
            SharedMcpTransport::Url { url } => {
                McpServer::Http(McpServerHttp::new(server.name.clone(), url))
            }
        })
        .collect();
    // A server you added under the same name is yours and wins.
    let name_taken = servers.iter().any(|server| {
        server
            .name
            .eq_ignore_ascii_case(velocity_collapse::MCP_SERVER_NAME)
    });
    if settings.agents.collapse_mcp && !name_taken {
        listed.extend(collapse_server());
    }
    listed
}

/// Collapse's MCP server (Collapse C9) with the connection's read-only token, read from the OS
/// credential store on each launch so connecting or disconnecting reaches the next agent.
/// Collapse refuses writes to this token, so Velocity adds no restriction of its own.
fn collapse_server() -> Option<McpServer> {
    let credentials = match velocity_collapse::load_credentials() {
        Ok(credentials) => credentials?,
        Err(error) => {
            eprintln!("Collapse MCP server left out: {error}");
            return None;
        }
    };
    let authorization =
        HttpHeader::new("Authorization", format!("Bearer {}", credentials.mcp_token));
    Some(McpServer::Http(
        McpServerHttp::new(
            velocity_collapse::MCP_SERVER_NAME,
            velocity_collapse::MCP_URL,
        )
        .headers(vec![authorization]),
    ))
}
