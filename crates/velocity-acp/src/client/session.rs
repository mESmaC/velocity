//! Starting a session (§3.2 Start a thread) and answering permission requests (Rule 1).

use std::path::PathBuf;

use agent_client_protocol::Agent;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    ClientCapabilities, FileSystemCapabilities, Implementation, InitializeRequest, McpServer,
    NewSessionRequest, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionId, SessionModeId,
    SessionModeState, SetSessionModeRequest,
};

use crate::{
    PermissionKind, SessionModeKind, choose_permission, choose_session_mode, keep_first_by_name,
    mode_to_select,
};

/// What a thread's session starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSetup {
    /// The thread's worktree: the agent's working directory and `session/new`'s `cwd`.
    pub worktree: PathBuf,
    /// The AIRC MCP server, always passed once it exists (F14, M8). `None` until then.
    pub airc: Option<McpServer>,
    /// Settings → Agents → shared MCP servers, passed to every agent.
    pub shared_servers: Vec<McpServer>,
    /// The ids of the agent's session modes that skip its permission checks, as the provider
    /// layer classifies them.
    pub bypass_modes: Vec<String>,
}

/// `initialize` → `session/new` → `session/set_mode` where it changes the mode.
pub async fn start_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
) -> Result<SessionId, agent_client_protocol::Error> {
    connection
        .send_request(initialize_request())
        .block_task()
        .await?;
    let session = connection
        .send_request(
            NewSessionRequest::new(setup.worktree.clone()).mcp_servers(session_servers(setup)),
        )
        .block_task()
        .await?;
    if let Some(mode) = bypass_mode(session.modes.as_ref(), &setup.bypass_modes) {
        connection
            .send_request(SetSessionModeRequest::new(session.session_id.clone(), mode))
            .block_task()
            .await?;
    }
    Ok(session.session_id)
}

fn initialize_request() -> InitializeRequest {
    let files = FileSystemCapabilities::new()
        .read_text_file(true)
        .write_text_file(true);
    InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(ClientCapabilities::new().fs(files).terminal(true))
        .client_info(Implementation::new("velocity", env!("CARGO_PKG_VERSION")))
}

/// AIRC, then the shared servers, keeping the first server of each name.
pub fn session_servers(setup: &SessionSetup) -> Vec<McpServer> {
    let servers: Vec<McpServer> = setup
        .airc
        .iter()
        .cloned()
        .chain(setup.shared_servers.iter().cloned())
        .collect();
    let keep: Vec<bool> = {
        let names: Vec<Option<&str>> = servers.iter().map(server_name).collect();
        keep_first_by_name(&names)
            .into_iter()
            .zip(&names)
            // A transport this build does not know has no name to compare, so it is kept.
            .map(|(first, name)| first || name.is_none())
            .collect()
    };
    servers
        .into_iter()
        .zip(keep)
        .filter_map(|(server, keep)| keep.then_some(server))
        .collect()
}

fn server_name(server: &McpServer) -> Option<&str> {
    match server {
        McpServer::Http(server) => Some(&server.name),
        McpServer::Sse(server) => Some(&server.name),
        McpServer::Stdio(server) => Some(&server.name),
        _ => None,
    }
}

/// The mode `session/set_mode` selects: the agent's bypass mode, unless it is already on.
fn bypass_mode(modes: Option<&SessionModeState>, bypass_ids: &[String]) -> Option<SessionModeId> {
    let state = modes?;
    let kinds: Vec<SessionModeKind> = state
        .available_modes
        .iter()
        .map(|mode| {
            if bypass_ids.iter().any(|id| id.as_str() == &*mode.id.0) {
                SessionModeKind::Bypass
            } else {
                SessionModeKind::Other
            }
        })
        .collect();
    let current = state
        .available_modes
        .iter()
        .position(|mode| mode.id == state.current_mode_id);
    let chosen = mode_to_select(choose_session_mode(&kinds), current)?;
    state
        .available_modes
        .get(chosen)
        .map(|mode| mode.id.clone())
}

/// Every permission request is answered at once and never shown (Rule 1): allow-always,
/// else allow-once. `Cancelled` only when the agent offers no allow option at all.
pub fn answer_permission(request: &RequestPermissionRequest) -> RequestPermissionResponse {
    let kinds: Vec<PermissionKind> = request
        .options
        .iter()
        .map(|option| permission_kind(option.kind))
        .collect();
    let outcome = match choose_permission(&kinds).and_then(|index| request.options.get(index)) {
        Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            option.option_id.clone(),
        )),
        None => RequestPermissionOutcome::Cancelled,
    };
    RequestPermissionResponse::new(outcome)
}

fn permission_kind(kind: PermissionOptionKind) -> PermissionKind {
    match kind {
        PermissionOptionKind::AllowOnce => PermissionKind::AllowOnce,
        PermissionOptionKind::AllowAlways => PermissionKind::AllowAlways,
        PermissionOptionKind::RejectOnce => PermissionKind::RejectOnce,
        PermissionOptionKind::RejectAlways => PermissionKind::RejectAlways,
        // A kind this build does not know is never chosen: only known allow options are.
        _ => PermissionKind::RejectOnce,
    }
}
