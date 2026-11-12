//! Starting a session (§3.2 Start a thread) and answering permission requests (Rule 1).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::Agent;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    AuthCapabilities, ClientCapabilities, FileSystemCapabilities, Implementation,
    InitializeRequest, LoadSessionRequest, McpCapabilities, McpServer, NewSessionRequest,
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigOption, SessionId,
    SessionModeId, SessionModeState, SetSessionModeRequest,
};

use super::config;
use crate::{
    PermissionKind, RunSettings, SessionModeKind, choose_permission, choose_session_mode,
    keep_first_by_name, mode_to_select,
};

/// What a thread's session starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSetup {
    /// The thread's worktree: the agent's working directory and `session/new`'s `cwd`.
    pub worktree: PathBuf,
    /// The AIRC MCP server, always passed once it exists (F14). AIRC ships in M8; until then
    /// threads start with `None` so the launch path works without it.
    pub airc: Option<McpServer>,
    /// Settings → Agents → shared MCP servers, passed to every agent.
    pub shared_servers: Vec<McpServer>,
    /// The ids of the agent's session modes that skip its permission checks, as the provider
    /// layer classifies them.
    pub bypass_modes: Vec<String>,
    /// Set when a parked thread resumes (SPEC.md §2.2): its earlier session and transcript.
    pub resume: Option<SessionResume>,
    /// The model, effort and speed the session runs on (F15), set through the agent's config
    /// options once the session starts. `None` leaves the agent's own defaults.
    pub run_settings: Option<RunSettings<String>>,
}

/// How a parked thread's agent picks up where it stopped: `session/load` of its earlier
/// session where the agent supports it, else a new session that is sent the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionResume {
    pub session_id: Option<String>,
    pub transcript: String,
    /// Older records may have only a provider session id and need its replayed history.
    pub replay_history: bool,
}

impl SessionResume {
    pub fn new(session_id: Option<String>, transcript: String) -> Self {
        Self {
            session_id,
            transcript,
            replay_history: false,
        }
    }

    pub fn with_history_replay(mut self, replay: bool) -> Self {
        self.replay_history = replay;
        self
    }
}

/// The session the agent runs, and whether it is the earlier one loaded again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartedSession {
    pub session_id: SessionId,
    pub loaded: bool,
    /// The agent's config options once the run settings were applied.
    pub options: Vec<SessionConfigOption>,
}

impl SessionSetup {
    pub fn new(worktree: PathBuf, bypass_modes: Vec<String>) -> Self {
        Self {
            worktree,
            airc: None,
            shared_servers: Vec::new(),
            bypass_modes,
            resume: None,
            run_settings: None,
        }
    }

    /// The same setup running on `settings` (F15).
    pub fn with_run_settings(mut self, settings: RunSettings<String>) -> Self {
        self.run_settings = Some(settings);
        self
    }
}

/// `initialize` → `session/new` → `session/set_mode` where it changes the mode.
pub async fn start_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
) -> Result<SessionId, agent_client_protocol::Error> {
    let initialized = connection
        .send_request(initialize_request())
        .block_task()
        .await?;
    let servers = supported_servers(
        session_servers(setup),
        &initialized.agent_capabilities.mcp_capabilities,
    );
    let session = connection
        .send_request(NewSessionRequest::new(setup.worktree.clone()).mcp_servers(servers))
        .block_task()
        .await?;
    select_bypass_mode(
        connection,
        &session.session_id,
        session.modes.as_ref(),
        setup,
    )
    .await?;
    Ok(session.session_id)
}

/// Like [`start_session`], but a resumed thread first tries `session/load` of its earlier
/// session. `replaying` suppresses replay only when the thread already holds its history.
pub async fn start_or_resume_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    replaying: &AtomicBool,
) -> Result<StartedSession, agent_client_protocol::Error> {
    let initialized = connection
        .send_request(initialize_request())
        .block_task()
        .await?;
    let earlier = setup
        .resume
        .as_ref()
        .and_then(|resume| resume.session_id.clone())
        .filter(|_| initialized.agent_capabilities.load_session);
    let servers = supported_servers(
        session_servers(setup),
        &initialized.agent_capabilities.mcp_capabilities,
    );
    if let Some(earlier) = earlier
        && let Some(started) =
            load_earlier_session(connection, setup, replaying, earlier, &servers).await?
    {
        return Ok(started);
    }
    new_resumed_session(connection, setup, servers).await
}

async fn load_earlier_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    replaying: &AtomicBool,
    earlier: String,
    servers: &[McpServer],
) -> Result<Option<StartedSession>, agent_client_protocol::Error> {
    let session_id = SessionId::new(earlier);
    let suppress_history = !setup
        .resume
        .as_ref()
        .is_some_and(|resume| resume.replay_history);
    replaying.store(suppress_history, Ordering::Release);
    let loaded = connection
        .send_request(
            LoadSessionRequest::new(session_id.clone(), setup.worktree.clone())
                .mcp_servers(servers.to_vec()),
        )
        .block_task()
        .await;
    replaying.store(false, Ordering::Release);
    // A session the agent no longer has falls back to a new one with the transcript.
    if let Ok(response) = loaded {
        select_bypass_mode(connection, &session_id, response.modes.as_ref(), setup).await?;
        let options = configure(connection, &session_id, response.config_options, setup).await?;
        return Ok(Some(StartedSession {
            session_id,
            loaded: true,
            options,
        }));
    }
    Ok(None)
}

async fn new_resumed_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    servers: Vec<McpServer>,
) -> Result<StartedSession, agent_client_protocol::Error> {
    let session = connection
        .send_request(NewSessionRequest::new(setup.worktree.clone()).mcp_servers(servers))
        .block_task()
        .await?;
    select_bypass_mode(
        connection,
        &session.session_id,
        session.modes.as_ref(),
        setup,
    )
    .await?;
    let options = configure(
        connection,
        &session.session_id,
        session.config_options,
        setup,
    )
    .await?;
    Ok(StartedSession {
        session_id: session.session_id,
        loaded: false,
        options,
    })
}

/// Applies the setup's run settings to the session's config options (F15).
async fn configure(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    options: Option<Vec<SessionConfigOption>>,
    setup: &SessionSetup,
) -> Result<Vec<SessionConfigOption>, agent_client_protocol::Error> {
    let options = options.unwrap_or_default();
    match &setup.run_settings {
        Some(settings) => config::apply_settings(connection, session_id, options, settings).await,
        None => Ok(options),
    }
}

async fn select_bypass_mode(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    modes: Option<&SessionModeState>,
    setup: &SessionSetup,
) -> Result<(), agent_client_protocol::Error> {
    if let Some(mode) = bypass_mode(modes, &setup.bypass_modes) {
        connection
            .send_request(SetSessionModeRequest::new(session_id.clone(), mode))
            .block_task()
            .await?;
    }
    Ok(())
}

pub(super) fn initialize_request() -> InitializeRequest {
    let files = FileSystemCapabilities::new()
        .read_text_file(true)
        .write_text_file(true);
    InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(
            ClientCapabilities::new()
                .fs(files)
                .terminal(true)
                // Terminal sign-in methods are run by Velocity (F15 Sign in).
                .auth(AuthCapabilities::new().terminal(true)),
        )
        .client_info(Implementation::new("velocity", env!("CARGO_PKG_VERSION")))
}

/// AIRC, the code graph beside it (F19), then the shared servers, keeping the first server of
/// each name.
pub fn session_servers(setup: &SessionSetup) -> Vec<McpServer> {
    let graph = setup
        .airc
        .as_ref()
        .and_then(|airc| super::airc::graph_server(airc, &setup.worktree));
    let servers: Vec<McpServer> = setup
        .airc
        .iter()
        .cloned()
        .chain(graph)
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

/// The servers the agent can reach: every agent speaks stdio, but HTTP and SSE servers go
/// only to agents that advertise them, since an unknown transport fails `session/new` and
/// would stop the whole session over one shared server.
fn supported_servers(servers: Vec<McpServer>, capabilities: &McpCapabilities) -> Vec<McpServer> {
    servers
        .into_iter()
        .filter(|server| match server {
            McpServer::Http(_) => capabilities.http,
            McpServer::Sse(_) => capabilities.sse,
            _ => true,
        })
        .collect()
}

pub(super) fn server_name(server: &McpServer) -> Option<&str> {
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
