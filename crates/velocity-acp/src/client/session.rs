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
    SubagentCapabilities, ToolCallUpdate, ToolKind,
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
    /// Sent ahead of the session's first prompt and not shown in the transcript: extra
    /// per-session preface lines outside the structured session card.
    pub preface: Option<String>,
    /// Short per-thread session card holding only values that change per thread (#534).
    pub session_card: Option<super::session_card::SessionCard>,
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
    /// Structured task brief prepared at dispatch or fix-forward handoff (#539).
    pub task_brief: Option<String>,
    /// Per-project agent memory folder keyed by project rather than workspace path (#539).
    pub project_memory_dir: Option<PathBuf>,
    /// Queued commands persisted from before a Velocity restart (#516).
    pub restored_runs: Vec<velocity_core::queued_runs::QueuedRunRecord>,
    /// Recent finished queued runs persisted on this thread for `run_status` and restart recovery (#536).
    pub run_history: Vec<velocity_core::queued_runs::QueuedRunRecord>,
    /// Durable persistence hook for `queue_run` and `run_cancel` acknowledgements (#664).
    pub queued_run_store: Option<super::queued_runs::QueuedRunStore>,
    /// Thread role that determines which MCP tools this session receives (#533).
    pub role: velocity_core::tool_policy::ThreadRole,
    /// True for read-only question threads: disables file write capabilities and write tools (#533).
    pub read_only: bool,
    /// True when the provider harness supports on-demand occasional tool loading (#533).
    pub lazy_tools: bool,
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
    /// Shared/Velocity MCP servers omitted because the agent lacks their transport (#534).
    pub(super) dropped_servers: Vec<String>,
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
            session_card: None,
            graph_task_title: None,
            pause_on_subscription_limit: false,
            usage_role: None,
            handoff: None,
            handoff_context: None,
            task_brief: None,
            project_memory_dir: None,
            restored_runs: Vec::new(),
            run_history: Vec::new(),
            queued_run_store: None,
            role: velocity_core::tool_policy::ThreadRole::Builder,
            read_only: false,
            lazy_tools: false,
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
        let mut settings = serde_json::json!({
            "deniedMcpServers": [{ "serverName": "computer-use" }]
        });
        if let Some(memory_dir) = &self.project_memory_dir
            && let Some(object) = settings.as_object_mut()
        {
            object.insert(
                "autoMemoryDirectory".into(),
                serde_json::json!(memory_dir.to_string_lossy()),
            );
            object.insert("autoMemoryEnabled".into(), serde_json::json!(true));
        }
        let mut disallowed = vec!["mcp__computer-use"];
        if self.read_only {
            disallowed.extend([
                "Bash",
                "Edit",
                "MultiEdit",
                "Write",
                "NotebookEdit",
                "KillShell",
            ]);
        }
        let agents_path = self.worktree.join("AGENTS.md");
        let instructions = match tokio::fs::read_to_string(&agents_path).await {
            Ok(instructions) => Some(instructions),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let inject_agents = instructions
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .filter(|text| !claude_harness_loads_agents(&self.worktree, text));
        if inject_agents.is_some()
            && let Some(object) = settings.as_object_mut()
        {
            // Exclude root AGENTS.md from Claude's own project-instruction discovery when
            // Velocity injects it via systemPrompt.append so Claude receives AGENTS.md once (#534).
            object.insert(
                "claudeMdExcludes".into(),
                serde_json::json!(claude_agents_excludes(&agents_path)),
            );
        }
        let mut metadata = Meta::from_iter([(
            "claudeCode".into(),
            serde_json::json!({
                "options": {
                    "settings": settings,
                    "disallowedTools": disallowed
                }
            }),
        )]);
        if let Some(instructions) = inject_agents {
            // claude-agent-acp forwards the preset's append field to the Claude SDK.
            metadata.insert(
                "systemPrompt".into(),
                serde_json::json!({
                    "type": "preset",
                    "preset": "claude_code",
                    "append": format!("Project instructions from AGENTS.md:\n\n{instructions}"),
                }),
            );
        }
        self.session_metadata = Some(metadata);
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
        .send_request(initialize_request_with_write(!shared.read_only))
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
        .send_request(thread_initialize_request(shared))
        .block_task()
        .await?;
    let earlier = setup
        .resume
        .as_ref()
        .and_then(|resume| resume.session_id.clone())
        .filter(|_| initialized.agent_capabilities.load_session);
    let (servers, dropped_servers) = split_supported_servers(
        session_servers(setup),
        &initialized.agent_capabilities.mcp_capabilities,
    );
    if let Some(earlier) = earlier
        && let Some(started) = load_earlier_session(
            connection,
            setup,
            shared,
            earlier,
            &servers,
            &dropped_servers,
        )
        .await?
    {
        return Ok(started);
    }
    new_resumed_session(connection, setup, shared, servers, dropped_servers).await
}

async fn load_earlier_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    shared: &Shared,
    earlier: String,
    servers: &[McpServer],
    dropped_servers: &[String],
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
            dropped_servers: dropped_servers.to_vec(),
        }));
    }
    Ok(None)
}

async fn new_resumed_session(
    connection: &ConnectionTo<Agent>,
    setup: &SessionSetup,
    shared: &Shared,
    servers: Vec<McpServer>,
    dropped_servers: Vec<String>,
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
        dropped_servers,
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

#[derive(Clone, Copy)]
enum ModePolicy {
    Bypass,
    ReadOnly,
}

impl ModePolicy {
    fn strict(self, shared: &Shared, target_modes: &[&str]) -> bool {
        match self {
            Self::Bypass => has_provider_bypass(shared),
            Self::ReadOnly => !target_modes.is_empty(),
        }
    }

    fn missing_reason(self) -> &'static str {
        match self {
            Self::Bypass => "the agent offers no recognized bypass mode",
            Self::ReadOnly => "the agent offers no recognized read-only mode",
        }
    }

    fn unchanged_reason(self) -> &'static str {
        match self {
            Self::Bypass => "the mode change did not enable bypass",
            Self::ReadOnly => "the mode change did not enable read-only mode",
        }
    }

    fn error(self, shared: &Shared, mode: &str, reason: impl std::fmt::Display) -> Error {
        let requirement = match self {
            Self::Bypass => "Velocity requires unrestricted access",
            Self::ReadOnly => "Read-only question threads require read-only access",
        };
        let message = format!("The agent reported mode `{mode}`. {requirement} ({reason}).");
        shared.emit(AgentEvent::Log(message.clone()));
        Error::new(ErrorCode::INTERNAL_ERROR, message)
    }
}

fn session_mode_targets<'a>(
    setup: &'a SessionSetup,
    shared: &Shared,
) -> (ModePolicy, Vec<&'a str>) {
    if setup.read_only {
        (
            ModePolicy::ReadOnly,
            crate::providers::read_only_modes(shared.provider).to_vec(),
        )
    } else {
        (
            ModePolicy::Bypass,
            setup.bypass_modes.iter().map(String::as_str).collect(),
        )
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
    let (policy, targets) = session_mode_targets(setup, shared);
    if targets.is_empty() {
        return Ok(());
    }
    if let Some((id, mode)) = config::mode_option(options) {
        let current = mode.current_value.0.as_ref();
        if is_target_mode(&targets, current) {
            return Ok(());
        }
        let Some((value, _)) = config::select_values(&mode.options)
            .into_iter()
            .find(|(value, _)| is_target_mode(&targets, value))
        else {
            if !policy.strict(shared, &targets) {
                return select_legacy_mode(connection, session_id, modes, &targets, policy, shared)
                    .await;
            }
            return Err(policy.error(shared, current, policy.missing_reason()));
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
        .map_err(|error| policy.error(shared, current, error))?;
        let Some((_, effective)) = config::mode_option(&updated) else {
            return Err(policy.error(shared, current, "the agent did not confirm the mode change"));
        };
        if !is_target_mode(&targets, &effective.current_value.0) {
            return Err(policy.error(
                shared,
                &effective.current_value.0,
                policy.unchanged_reason(),
            ));
        }
        *options = updated;
        return Ok(());
    }
    select_legacy_mode(connection, session_id, modes, &targets, policy, shared).await
}

async fn select_legacy_mode(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    modes: Option<&SessionModeState>,
    target_modes: &[&str],
    policy: ModePolicy,
    shared: &Shared,
) -> Result<(), Error> {
    let Some(modes) = modes else { return Ok(()) };
    let current = modes.current_mode_id.0.as_ref();
    if is_target_mode(target_modes, current) {
        return Ok(());
    }
    let Some(mode) = matching_mode(modes, target_modes) else {
        if !policy.strict(shared, target_modes) {
            return Ok(());
        }
        return Err(policy.error(shared, current, policy.missing_reason()));
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
    result.map_err(|error| policy.error(shared, reported.as_deref().unwrap_or(current), error))?;
    match reported {
        Some(mode) if !is_target_mode(target_modes, &mode) => {
            Err(policy.error(shared, &mode, policy.unchanged_reason()))
        }
        // The legacy response has no mode field. Codex must confirm a change since an
        // empty success can leave the previous sandbox or full-access mode active.
        None if shared.provider == Some(Provider::Codex) => {
            Err(policy.error(shared, current, "the agent did not confirm the mode change"))
        }
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

fn is_target_mode(target_modes: &[&str], mode: &str) -> bool {
    target_modes.contains(&mode)
}

fn has_provider_bypass(shared: &Shared) -> bool {
    // Custom agents may use modes for unrelated features and have no provider-defined bypass.
    shared
        .provider
        .is_some_and(|provider| !matches!(provider, Provider::Custom(_)))
}

pub(super) fn initialize_request() -> InitializeRequest {
    initialize_request_with_write(true)
}

fn initialize_request_with_write(allow_write: bool) -> InitializeRequest {
    let files = FileSystemCapabilities::new()
        .read_text_file(true)
        .write_text_file(allow_write);
    InitializeRequest::new(ProtocolVersion::V1)
        .client_capabilities(
            ClientCapabilities::new()
                .fs(files)
                .terminal(allow_write)
                // Terminal sign-in methods are run by Velocity (F15 Sign in).
                .auth(AuthCapabilities::new().terminal(true)),
        )
        .client_info(Implementation::new("velocity", env!("CARGO_PKG_VERSION")))
}

/// A thread session's `initialize`. Codex is told Velocity takes native subagent sessions:
/// codex-acp forwards a child thread's own items only to a client that declares them (§3.2
/// Subagents). Every other agent keeps reporting subagents inside the parent session.
fn thread_initialize_request(shared: &Shared) -> InitializeRequest {
    let mut request = initialize_request_with_write(!shared.read_only);
    if shared.provider == Some(Provider::Codex) {
        request.client_capabilities = request
            .client_capabilities
            .subagents(SubagentCapabilities::new());
    }
    request
}

/// AIRC, the code graph beside it (F19), then the shared servers, keeping the first server of
/// each name. Role and lazy-loading flags from `setup` are applied to Velocity's own servers (#533).
pub fn session_servers(setup: &SessionSetup) -> Vec<McpServer> {
    let airc = setup
        .airc
        .as_ref()
        .map(|airc| super::airc::with_airc_policy(airc, setup.role, setup.lazy_tools));
    let graph = setup.airc.as_ref().and_then(|airc| {
        super::airc::graph_server_for_role(
            airc,
            &setup.worktree,
            Some(setup.role),
            setup.lazy_tools,
        )
    });
    let shared_servers = setup
        .shared_servers
        .iter()
        .map(|server| apply_shared_server_lazy(server, setup.lazy_tools));
    let servers: Vec<McpServer> = airc
        .into_iter()
        .chain(graph)
        .chain(shared_servers)
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

const BROWSER_MCP_SWITCH: &str = "--browser-mcp";
const BROWSER_LAZY_FLAG: &str = "--lazy";

fn apply_shared_server_lazy(server: &McpServer, lazy: bool) -> McpServer {
    let McpServer::Stdio(stdio) = server else {
        return server.clone();
    };
    match stdio.args.first().map(String::as_str) {
        Some(velocity_rosen::mcp_proxy::SERVER_SWITCH) if lazy => {
            if stdio
                .args
                .iter()
                .any(|arg| arg == velocity_rosen::mcp_filter::LAZY_FLAG)
            {
                return server.clone();
            }
            let mut updated = stdio.clone();
            updated
                .args
                .insert(1, velocity_rosen::mcp_filter::LAZY_FLAG.to_owned());
            McpServer::Stdio(updated)
        }
        // Browser servers pass `--lazy` for non-UI implementation threads; strip it when
        // the provider harness does not support deferred tool discovery (#533, #689).
        Some(BROWSER_MCP_SWITCH) if !lazy => {
            if !stdio.args.iter().any(|arg| arg == BROWSER_LAZY_FLAG) {
                return server.clone();
            }
            let mut updated = stdio.clone();
            updated.args.retain(|arg| arg != BROWSER_LAZY_FLAG);
            McpServer::Stdio(updated)
        }
        _ => server.clone(),
    }
}

/// The servers the agent can reach: every agent speaks stdio, but HTTP and SSE servers go
/// only to agents that advertise them, since an unknown transport fails `session/new` and
/// would stop the whole session over one shared server.
fn supported_servers(servers: Vec<McpServer>, capabilities: &McpCapabilities) -> Vec<McpServer> {
    split_supported_servers(servers, capabilities).0
}

fn split_supported_servers(
    servers: Vec<McpServer>,
    capabilities: &McpCapabilities,
) -> (Vec<McpServer>, Vec<String>) {
    let mut kept = Vec::with_capacity(servers.len());
    let mut dropped = Vec::new();
    for server in servers {
        let supported = match &server {
            McpServer::Http(_) => capabilities.http,
            McpServer::Sse(_) => capabilities.sse,
            _ => true,
        };
        if supported {
            kept.push(server);
        } else if let Some(name) = server_name(&server) {
            dropped.push(name.to_owned());
        }
    }
    (kept, dropped)
}

pub(super) fn server_name(server: &McpServer) -> Option<&str> {
    match server {
        McpServer::Http(server) => Some(&server.name),
        McpServer::Sse(server) => Some(&server.name),
        McpServer::Stdio(server) => Some(&server.name),
        _ => None,
    }
}

/// The mode `session/set_mode` selects: the first matching target mode, unless active.
fn matching_mode(state: &SessionModeState, target_modes: &[&str]) -> Option<SessionModeId> {
    let kinds: Vec<SessionModeKind> = state
        .available_modes
        .iter()
        .map(|mode| {
            if is_target_mode(target_modes, &mode.id.0) {
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

/// Answers `session/request_permission` on read-only question threads (#687): allows
/// repository reads, search/fetch/think and Velocity's read-only MCP tools, and rejects
/// file writes, shell execution, mode switches and unrecognized tools.
pub fn answer_read_only_permission(
    request: &RequestPermissionRequest,
) -> RequestPermissionResponse {
    let kinds: Vec<PermissionKind> = request
        .options
        .iter()
        .map(|option| permission_kind(option.kind))
        .collect();
    let allowed = is_read_only_tool_call(&request.tool_call);
    let chosen = choose_read_only_permission(&kinds, allowed);
    let outcome = match chosen.and_then(|index| request.options.get(index)) {
        Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            option.option_id.clone(),
        )),
        None => RequestPermissionOutcome::Cancelled,
    };
    RequestPermissionResponse::new(outcome)
}

fn choose_read_only_permission(options: &[PermissionKind], allowed: bool) -> Option<usize> {
    let (preferred, fallback) = if allowed {
        (PermissionKind::AllowOnce, PermissionKind::AllowAlways)
    } else {
        (PermissionKind::RejectOnce, PermissionKind::RejectAlways)
    };
    options
        .iter()
        .position(|kind| *kind == preferred)
        .or_else(|| options.iter().position(|kind| *kind == fallback))
}

fn is_read_only_tool_call(tool_call: &ToolCallUpdate) -> bool {
    match tool_call.fields.kind {
        Some(ToolKind::Read | ToolKind::Search | ToolKind::Think | ToolKind::Fetch) => true,
        Some(
            ToolKind::Edit
            | ToolKind::Delete
            | ToolKind::Move
            | ToolKind::Execute
            | ToolKind::SwitchMode,
        ) => false,
        None | Some(ToolKind::Other) | Some(_) => {
            read_only_tool_name(tool_call).is_some_and(is_allowed_read_only_tool)
        }
    }
}

fn read_only_tool_name(tool_call: &ToolCallUpdate) -> Option<&str> {
    tool_call
        .meta
        .as_ref()
        .and_then(|meta| meta.get("claudeCode"))
        .and_then(|claude| claude.get("toolName"))
        .and_then(serde_json::Value::as_str)
        .or(tool_call.fields.title.as_deref())
}

fn is_allowed_read_only_tool(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    if let Some((server, tool)) = split_mcp_tool_name(&normalized) {
        return matches!(
            server,
            "airc" | "velocity-graph" | "velocity_graph" | "rosen" | "collapse"
        ) && is_tool_identifier(tool);
    }
    matches!(
        normalized.as_str(),
        "read"
            | "read_file"
            | "read_text_file"
            | "view_file"
            | "view_file_outline"
            | "view_code_item"
            | "glob"
            | "grep"
            | "grep_search"
            | "find_by_name"
            | "list_dir"
            | "ls"
            | "web_search"
            | "web_fetch"
            | "websearch"
            | "webfetch"
            | "todo_read"
            | "todoread"
    )
}

fn split_mcp_tool_name(name: &str) -> Option<(&str, &str)> {
    if let Some(rest) = name.strip_prefix("mcp__") {
        return rest.split_once("__");
    }
    name.split_once('/').or_else(|| name.split_once(':'))
}

fn is_tool_identifier(tool: &str) -> bool {
    !tool.is_empty()
        && tool
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
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

/// True when Claude Code's own project instruction files (`CLAUDE.md`, `.claude/CLAUDE.md`)
/// already import or duplicate `AGENTS.md`, so Velocity must not append a second copy (#534).
fn claude_harness_loads_agents(worktree: &std::path::Path, agents_contents: &str) -> bool {
    let normalized_agents = agents_contents.trim();
    for relative in ["CLAUDE.md", ".claude/CLAUDE.md"] {
        let Ok(contents) = std::fs::read_to_string(worktree.join(relative)) else {
            continue;
        };
        if claude_md_includes_agents(&contents, normalized_agents) {
            return true;
        }
    }
    false
}

fn claude_md_includes_agents(claude_md: &str, normalized_agents: &str) -> bool {
    if !normalized_agents.is_empty() && claude_md.trim() == normalized_agents {
        return true;
    }
    claude_md.lines().any(|line| {
        let trimmed = line.trim();
        matches!(
            trimmed,
            "@AGENTS.md" | "@./AGENTS.md" | "@../AGENTS.md" | "@agents.md" | "@./agents.md"
        )
    })
}

fn claude_agents_excludes(agents_path: &std::path::Path) -> Vec<String> {
    let forward = agents_path.to_string_lossy().replace('\\', "/");
    let mut excludes = vec!["AGENTS.md".to_owned()];
    if !forward.is_empty() && !excludes.contains(&forward) {
        excludes.push(forward);
    }
    excludes
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempWorktree(PathBuf);

    impl TempWorktree {
        fn new() -> std::io::Result<Self> {
            let path = std::env::temp_dir().join(format!(
                "velocity-acp-session-test-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TempWorktree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn claude_injects_agents_once_and_excludes_native_duplicate() -> std::io::Result<()> {
        let dir = TempWorktree::new()?;
        std::fs::write(dir.0.join("AGENTS.md"), "# AGENTS.md\n- Rule one.\n")?;
        let mut setup = SessionSetup::new(dir.0.clone(), Vec::new());
        setup.load_claude_project_instructions().await?;
        let first_bytes = setup.instruction_bytes();
        assert!(first_bytes > 0);
        // Calling a second time must be idempotent and never append a second copy.
        setup.load_claude_project_instructions().await?;
        assert_eq!(setup.instruction_bytes(), first_bytes);
        let meta = setup
            .session_metadata
            .as_ref()
            .ok_or_else(|| std::io::Error::other("missing Claude session metadata"))?;
        let excludes = &meta["claudeCode"]["options"]["settings"]["claudeMdExcludes"];
        assert!(
            excludes
                .as_array()
                .is_some_and(|list| list.iter().any(|item| item == "AGENTS.md"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn claude_skips_injection_when_claude_md_imports_agents() -> std::io::Result<()> {
        let dir = TempWorktree::new()?;
        std::fs::write(dir.0.join("AGENTS.md"), "# AGENTS.md\n- Rule one.\n")?;
        std::fs::write(dir.0.join("CLAUDE.md"), "@AGENTS.md\n")?;
        let mut setup = SessionSetup::new(dir.0.clone(), Vec::new());
        setup.load_claude_project_instructions().await?;
        assert_eq!(setup.instruction_bytes(), 0);
        let meta = setup
            .session_metadata
            .as_ref()
            .ok_or_else(|| std::io::Error::other("missing Claude session metadata"))?;
        assert!(
            meta["claudeCode"]["options"]["settings"]
                .get("claudeMdExcludes")
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn read_only_initialize_omits_write_and_terminal_capabilities() {
        // Risk (#687): read-only question threads must retain repository reads while
        // omitting both fs.writeTextFile and unrestricted ACP terminal capabilities.
        let read_only = initialize_request_with_write(false);
        assert!(read_only.client_capabilities.fs.read_text_file);
        assert!(!read_only.client_capabilities.fs.write_text_file);
        assert!(!read_only.client_capabilities.terminal);
        assert!(read_only.client_capabilities.auth.terminal);

        let writable = initialize_request_with_write(true);
        assert!(writable.client_capabilities.fs.read_text_file);
        assert!(writable.client_capabilities.fs.write_text_file);
        assert!(writable.client_capabilities.terminal);
    }

    #[tokio::test]
    async fn claude_read_only_disallows_shell_and_file_write_tools() -> std::io::Result<()> {
        // Risk (#687): Claude Code's native Bash and edit/write tools must be disallowed on
        // read-only question sessions so the agent cannot mutate the project repository.
        let dir = TempWorktree::new()?;
        let mut setup = SessionSetup::new(dir.0.clone(), Vec::new());
        setup.read_only = true;
        setup.load_claude_project_instructions().await?;
        let meta = setup
            .session_metadata
            .as_ref()
            .ok_or_else(|| std::io::Error::other("missing Claude session metadata"))?;
        let disallowed = meta["claudeCode"]["options"]["disallowedTools"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("missing disallowedTools array"))?;
        for tool in [
            "Bash",
            "Edit",
            "MultiEdit",
            "Write",
            "NotebookEdit",
            "KillShell",
        ] {
            assert!(
                disallowed.iter().any(|entry| entry == tool),
                "expected `{tool}` in read-only disallowedTools"
            );
        }
        Ok(())
    }

    #[test]
    fn read_only_permissions_allow_reads_and_reject_writes_or_commands() {
        // Risk (#687): read-only question sessions must approve repository reads and
        // Velocity's role-filtered MCP tools while rejecting native writes, shell execution,
        // mode switches, and unknown tools.
        use agent_client_protocol::v1::schema::{
            PermissionOption, ToolCallId, ToolCallUpdateFields,
        };

        let options = || {
            vec![
                PermissionOption::new("allow-once", "Allow once", PermissionOptionKind::AllowOnce),
                PermissionOption::new(
                    "allow-always",
                    "Allow always",
                    PermissionOptionKind::AllowAlways,
                ),
                PermissionOption::new(
                    "reject-once",
                    "Reject once",
                    PermissionOptionKind::RejectOnce,
                ),
            ]
        };
        let make_request = |kind: Option<ToolKind>, title: Option<&str>| {
            let mut fields = ToolCallUpdateFields::new();
            if let Some(kind) = kind {
                fields = fields.kind(kind);
            }
            if let Some(title) = title {
                fields = fields.title(title.to_owned());
            }
            RequestPermissionRequest::new(
                SessionId::new("session-1"),
                ToolCallUpdate::new(ToolCallId::new("call-1"), fields),
                options(),
            )
        };
        let selected_id = |response: RequestPermissionResponse| match response.outcome {
            RequestPermissionOutcome::Selected(selected) => Some(selected.option_id.0.to_string()),
            _ => None,
        };

        assert_eq!(
            selected_id(answer_read_only_permission(&make_request(
                Some(ToolKind::Read),
                Some("Read src/lib.rs"),
            ))),
            Some("allow-once".to_owned())
        );
        assert_eq!(
            selected_id(answer_read_only_permission(&make_request(
                Some(ToolKind::Search),
                Some("Grep"),
            ))),
            Some("allow-once".to_owned())
        );
        assert_eq!(
            selected_id(answer_read_only_permission(&make_request(
                Some(ToolKind::Other),
                Some("mcp__velocity-graph__graph_find"),
            ))),
            Some("allow-once".to_owned())
        );
        assert_eq!(
            selected_id(answer_read_only_permission(&make_request(
                Some(ToolKind::Execute),
                Some("Bash"),
            ))),
            Some("reject-once".to_owned())
        );
        assert_eq!(
            selected_id(answer_read_only_permission(&make_request(
                Some(ToolKind::Edit),
                Some("Edit"),
            ))),
            Some("reject-once".to_owned())
        );
        assert_eq!(
            selected_id(answer_read_only_permission(&make_request(
                Some(ToolKind::SwitchMode),
                Some("Switch mode"),
            ))),
            Some("reject-once".to_owned())
        );
        assert_eq!(
            selected_id(answer_read_only_permission(&make_request(
                Some(ToolKind::Other),
                Some("mcp__custom__delete_everything"),
            ))),
            Some("reject-once".to_owned())
        );
    }

    #[test]
    fn provider_read_only_launch_enforces_codex_and_antigravity_restrictions() {
        // Risk (#687): Codex question threads must not start in `agent-full-access`, and
        // Antigravity question threads must not pass `--dangerously-skip-permissions`.
        let mut codex = crate::provider_agent(Provider::Codex)
            .unwrap_or_else(|| unreachable!("Codex is built-in"));
        crate::providers::apply_read_only(&mut codex.launch, &mut codex.bypass_modes);
        assert!(codex.bypass_modes.is_empty());
        assert_eq!(
            codex.launch.env_value("INITIAL_AGENT_MODE"),
            Some("read-only")
        );
        let config_raw = codex.launch.env_value("CODEX_CONFIG").unwrap_or_default();
        assert!(config_raw.contains(r#""sandbox_mode":"read-only""#));
        assert!(config_raw.contains(r#""approval_policy":"on-request""#));

        let mut agy = crate::provider_agent(Provider::Antigravity)
            .unwrap_or_else(|| unreachable!("Antigravity is built-in"));
        crate::providers::apply_read_only(&mut agy.launch, &mut agy.bypass_modes);
        assert!(agy.bypass_modes.is_empty());
        assert!(
            !agy.launch
                .args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );
    }

    #[test]
    fn browser_mcp_keeps_lazy_only_when_harness_supports_deferred_discovery() {
        // Risk (#689): non-UI implementation and repair threads attach velocity-browser with
        // `--lazy`, which must be kept when the harness supports deferred discovery and
        // stripped otherwise so every harness still sees all F20 browser tools.
        let make_setup = |lazy_tools| {
            let mut setup = SessionSetup::new(PathBuf::from("."), Vec::new());
            setup.role = velocity_core::tool_policy::ThreadRole::FollowUp;
            setup.lazy_tools = lazy_tools;
            setup.shared_servers.push(McpServer::Stdio(
                agent_client_protocol::v1::schema::McpServerStdio::new(
                    "velocity-browser",
                    "velocity",
                )
                .args(vec![BROWSER_MCP_SWITCH.into(), BROWSER_LAZY_FLAG.into()]),
            ));
            setup
        };
        let browser_args = |setup: &SessionSetup| -> Vec<String> {
            session_servers(setup)
                .into_iter()
                .find_map(|server| match server {
                    McpServer::Stdio(stdio) if stdio.name == "velocity-browser" => Some(stdio.args),
                    _ => None,
                })
                .unwrap_or_default()
        };

        assert_eq!(
            browser_args(&make_setup(true)),
            vec![BROWSER_MCP_SWITCH, BROWSER_LAZY_FLAG]
        );
        assert_eq!(browser_args(&make_setup(false)), vec![BROWSER_MCP_SWITCH]);
    }
}
