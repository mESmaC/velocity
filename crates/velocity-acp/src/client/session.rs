//! Starting a session (§3.2 Start a thread) and answering permission requests (Rule 1).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    AuthCapabilities, ClientCapabilities, FileSystemCapabilities, Implementation,
    InitializeRequest, LoadSessionRequest, McpCapabilities, McpServer, Meta, NewSessionRequest,
    PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigOption, SessionId,
    SessionModeId, SessionModeState, SessionNotification, SessionUpdate, SetSessionModeRequest,
};
use agent_client_protocol::{Agent, Error, ErrorCode};
use velocity_graph::token_usage::Role;

use super::{AgentEvent, Shared, config};
use crate::session_config::ConfigValue;
use crate::{
    PermissionKind, Provider, RunSettings, SessionModeKind, choose_permission, choose_session_mode,
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
    /// Provider extensions sent at session setup, outside the transcript.
    session_metadata: Option<Meta>,
    /// The ids of the agent's session modes that skip its permission checks, as the provider
    /// layer classifies them.
    pub bypass_modes: Vec<String>,
    /// Set when a parked thread resumes (SPEC.md §2.2): its earlier session and transcript.
    pub resume: Option<SessionResume>,
    /// The model, effort and speed the session runs on (F15), set through the agent's config
    /// options once the session starts. `None` leaves the agent's own defaults.
    pub run_settings: Option<RunSettings<String>>,
    /// Sent ahead of the session's first prompt and not shown in the transcript: what the
    /// agent must know about its folder, such as a Rosen workspace's note (F5).
    pub preface: Option<String>,
    /// The work item's title for the new session's initial code graph lookup.
    pub graph_task_title: Option<String>,
    /// A saved build thread moves its pending input to a new account on quota failure.
    pub pause_on_subscription_limit: bool,
    /// Duty sessions can report a more specific usage role than AIRC membership supplies.
    pub usage_role: Option<Role>,
    /// A Rosen work item enrolled in the context hand-off experiment.
    pub handoff: Option<super::handoff::Experiment>,
    /// A fresh session's saved note and current change state have their own preface budget.
    pub handoff_context: Option<String>,
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
    pub(super) graph_on: bool,
}

impl SessionSetup {
    pub fn new(worktree: PathBuf, bypass_modes: Vec<String>) -> Self {
        Self {
            worktree,
            airc: None,
            shared_servers: Vec::new(),
            session_metadata: None,
            bypass_modes,
            resume: None,
            run_settings: None,
            preface: None,
            graph_task_title: None,
            pause_on_subscription_limit: false,
            usage_role: None,
            handoff: None,
            handoff_context: None,
        }
    }

    /// The same setup running on `settings` (F15).
    pub fn with_run_settings(mut self, settings: RunSettings<String>) -> Self {
        self.run_settings = Some(settings);
        self
    }

    pub fn with_graph_task_title(mut self, title: String) -> Self {
        self.graph_task_title = Some(title);
        self
    }

    pub(super) fn instruction_bytes(&self) -> usize {
        self.session_metadata
            .as_ref()
            .and_then(|meta| meta.get("systemPrompt"))
            .and_then(|prompt| prompt["append"].as_str())
            .map_or(0, str::len)
    }

    pub(super) async fn load_claude_project_instructions(&mut self) -> std::io::Result<()> {
        let instructions = match tokio::fs::read_to_string(self.worktree.join("AGENTS.md")).await {
            Ok(instructions) => instructions,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if !instructions.trim().is_empty() {
            // claude-agent-acp forwards the preset's append field to the Claude SDK.
            self.session_metadata = Some(Meta::from_iter([(
                "systemPrompt".into(),
                serde_json::json!({
                    "type": "preset",
                    "preset": "claude_code",
                    "append": format!("Project instructions from AGENTS.md:\n\n{instructions}"),
                }),
            )]));
        }
        Ok(())
    }

    fn new_session_request(&self, servers: Vec<McpServer>) -> NewSessionRequest {
        NewSessionRequest::new(self.worktree.clone())
            .mcp_servers(servers)
            .meta(self.session_metadata.clone())
    }
}

/// `initialize` → `session/new` → select and check the bypass mode.
pub(super) async fn start_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    shared: &Shared,
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
        .send_request(setup.new_session_request(servers))
        .block_task()
        .await?;
    select_bypass_mode(
        connection,
        &session.session_id,
        session.modes.as_ref(),
        setup,
        shared,
        &mut session.config_options.unwrap_or_default(),
    )
    .await?;
    Ok(session.session_id)
}

/// Like [`start_session`], but a resumed thread first tries `session/load` of its earlier
/// session. `replaying` suppresses replay only when the thread already holds its history.
pub(super) async fn start_or_resume_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    shared: &Shared,
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
            load_earlier_session(connection, setup, shared, earlier, &servers).await?
    {
        return Ok(started);
    }
    new_resumed_session(connection, setup, shared, servers).await
}

async fn load_earlier_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    shared: &Shared,
    earlier: String,
    servers: &[McpServer],
) -> Result<Option<StartedSession>, agent_client_protocol::Error> {
    let session_id = SessionId::new(earlier);
    let suppress_history = !setup
        .resume
        .as_ref()
        .is_some_and(|resume| resume.replay_history);
    shared.replaying.store(suppress_history, Ordering::Release);
    let loaded = connection
        .send_request(
            LoadSessionRequest::new(session_id.clone(), setup.worktree.clone())
                .mcp_servers(servers.to_vec())
                .meta(setup.session_metadata.clone()),
        )
        .block_task()
        .await;
    shared.replaying.store(false, Ordering::Release);
    // A session the agent no longer has falls back to a new one with the transcript.
    if let Ok(response) = loaded {
        let mut options = response.config_options.unwrap_or_default();
        select_bypass_mode(
            connection,
            &session_id,
            response.modes.as_ref(),
            setup,
            shared,
            &mut options,
        )
        .await?;
        let options = configure(connection, &session_id, options, setup).await?;
        return Ok(Some(StartedSession {
            session_id,
            loaded: true,
            options,
            graph_on: has_graph(servers),
        }));
    }
    Ok(None)
}

async fn new_resumed_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    shared: &Shared,
    servers: Vec<McpServer>,
) -> Result<StartedSession, agent_client_protocol::Error> {
    let graph_on = has_graph(&servers);
    let session = connection
        .send_request(setup.new_session_request(servers))
        .block_task()
        .await?;
    let mut options = session.config_options.unwrap_or_default();
    select_bypass_mode(
        connection,
        &session.session_id,
        session.modes.as_ref(),
        setup,
        shared,
        &mut options,
    )
    .await?;
    let options = configure(connection, &session.session_id, options, setup).await?;
    Ok(StartedSession {
        session_id: session.session_id,
        loaded: false,
        options,
        graph_on,
    })
}

fn has_graph(servers: &[McpServer]) -> bool {
    servers
        .iter()
        .any(|server| server_name(server) == Some(velocity_graph::SERVER_NAME))
}

/// Applies the setup's run settings to the session's config options (F15).
async fn configure(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    options: Vec<SessionConfigOption>,
    setup: &SessionSetup,
) -> Result<Vec<SessionConfigOption>, agent_client_protocol::Error> {
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
    shared: &Shared,
    options: &mut Vec<SessionConfigOption>,
) -> Result<(), agent_client_protocol::Error> {
    let bypass_ids = &setup.bypass_modes;
    if bypass_ids.is_empty() {
        return Ok(());
    }
    if let Some((id, mode)) = config::mode_option(options) {
        let current = mode.current_value.0.as_ref();
        if is_bypass(bypass_ids, current) {
            return Ok(());
        }
        let Some((value, _)) = config::select_values(&mode.options)
            .into_iter()
            .find(|(value, _)| is_bypass(bypass_ids, value))
        else {
            if !has_provider_bypass(shared) {
                return select_legacy_bypass(connection, session_id, modes, bypass_ids, shared)
                    .await;
            }
            return Err(restricted_mode(
                shared,
                current,
                "the agent offers no recognized bypass mode",
            ));
        };
        // Unlike session/set_mode, this response reports the effective mode even if
        // the agent accepted the request without changing its configuration.
        let updated = config::set_option(
            connection,
            session_id,
            Arc::clone(&id.0),
            ConfigValue::Select(value),
        )
        .await
        .map_err(|error| restricted_mode(shared, current, error))?;
        let Some((_, effective)) = config::mode_option(&updated) else {
            return Err(restricted_mode(
                shared,
                current,
                "the agent did not confirm the mode change",
            ));
        };
        if !is_bypass(bypass_ids, &effective.current_value.0) {
            return Err(restricted_mode(
                shared,
                &effective.current_value.0,
                "the mode change did not enable bypass",
            ));
        }
        *options = updated;
        return Ok(());
    }
    select_legacy_bypass(connection, session_id, modes, bypass_ids, shared).await
}

async fn select_legacy_bypass(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    modes: Option<&SessionModeState>,
    bypass_ids: &[String],
    shared: &Shared,
) -> Result<(), Error> {
    let Some(modes) = modes else { return Ok(()) };
    let current = modes.current_mode_id.0.as_ref();
    if is_bypass(bypass_ids, current) {
        return Ok(());
    }
    let Some(mode) = bypass_mode(modes, bypass_ids) else {
        if !has_provider_bypass(shared) {
            return Ok(());
        }
        return Err(restricted_mode(
            shared,
            current,
            "the agent offers no recognized bypass mode",
        ));
    };
    *shared
        .mode_selection
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(ModeSelection {
        session_id: session_id.clone(),
        reported: None,
    });
    let result = connection
        .send_request(SetSessionModeRequest::new(session_id.clone(), mode))
        .block_task()
        .await;
    let reported = shared
        .mode_selection
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .and_then(|selection| selection.reported);
    result
        .map_err(|error| restricted_mode(shared, reported.as_deref().unwrap_or(current), error))?;
    match reported {
        Some(mode) if !is_bypass(bypass_ids, &mode) => Err(restricted_mode(
            shared,
            &mode,
            "the mode change did not enable bypass",
        )),
        // The legacy response has no mode field. Codex must confirm a change from a
        // restricted mode since an empty success can leave its sandbox and reviewer active.
        None if shared.provider == Some(Provider::Codex) => Err(restricted_mode(
            shared,
            current,
            "the agent did not confirm the mode change",
        )),
        _ => Ok(()),
    }
}

pub(super) struct ModeSelection {
    session_id: SessionId,
    reported: Option<Arc<str>>,
}

/// Captures reports for the session whose legacy mode request is in flight.
pub(super) fn record_mode(shared: &Shared, notification: &SessionNotification) {
    let reported = match &notification.update {
        SessionUpdate::CurrentModeUpdate(update) => Some(&update.current_mode_id.0),
        SessionUpdate::ConfigOptionUpdate(update) => {
            config::mode_option(&update.config_options).map(|(_, mode)| &mode.current_value.0)
        }
        _ => None,
    };
    if let Some(reported) = reported
        && let Some(selection) = shared
            .mode_selection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_mut()
        && selection.session_id == notification.session_id
    {
        selection.reported = Some(Arc::clone(reported));
    }
}

fn is_bypass(bypass_ids: &[String], mode: &str) -> bool {
    bypass_ids.iter().any(|id| id == mode)
}

fn has_provider_bypass(shared: &Shared) -> bool {
    // Custom agents may use modes for unrelated features and have no provider-defined bypass.
    shared
        .provider
        .is_some_and(|provider| !matches!(provider, Provider::Custom(_)))
}

fn restricted_mode(shared: &Shared, mode: &str, reason: impl std::fmt::Display) -> Error {
    let message = format!(
        "The agent reported mode `{mode}`. Velocity requires unrestricted access ({reason})."
    );
    shared.emit(AgentEvent::Log(message.clone()));
    Error::new(ErrorCode::INTERNAL_ERROR, message)
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
fn bypass_mode(state: &SessionModeState, bypass_ids: &[String]) -> Option<SessionModeId> {
    let kinds: Vec<SessionModeKind> = state
        .available_modes
        .iter()
        .map(|mode| {
            if is_bypass(bypass_ids, &mode.id.0) {
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
