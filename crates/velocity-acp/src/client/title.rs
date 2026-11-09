//! Asking the title model for a thread's title (F4): one short session on the title model's
//! own agent, run while the thread's worktree is being taken.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    ContentBlock, NewSessionRequest, PromptRequest, SessionConfigId, SessionConfigKind,
    SessionConfigOption, SessionConfigOptionCategory, SessionConfigOptionValue,
    SessionConfigSelectOptions, SessionConfigValueId, SessionId, SessionUpdate,
    SetSessionConfigOptionRequest,
};
use agent_client_protocol::{Agent, ByteStreams, Client};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{
    AgentEvent, AgentLaunch, ClientError, Shared, forward_log, handlers, process, session,
};
use crate::RunSettings;
use crate::session_config::{
    ConfigCategory, ConfigChoices, ConfigOption, ConfigValue, choose_config_values,
};

/// What a title request runs with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleRequest {
    /// The title model's agent.
    pub launch: AgentLaunch,
    /// Where the agent starts: the project's repository. The title session reads nothing,
    /// but agents need a real folder for `session/new`.
    pub working_directory: PathBuf,
    /// Settings → Providers → Thread titles (F15).
    pub settings: RunSettings<String>,
    /// The whole prompt, from `velocity_core::thread_titles::title_request_prompt`.
    pub prompt: String,
    /// F4's two seconds. The agent is stopped when it passes.
    pub deadline: Duration,
}

impl TitleRequest {
    pub fn new(
        launch: AgentLaunch,
        working_directory: PathBuf,
        settings: RunSettings<String>,
        prompt: String,
        deadline: Duration,
    ) -> Self {
        TitleRequest {
            launch,
            working_directory,
            settings,
            prompt,
            deadline,
        }
    }
}

/// The model's raw reply and how long it took to arrive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitleReply {
    pub text: String,
    /// From starting the agent to the prompt's answer, before the agent is stopped: stopping
    /// a process tree takes a few hundred milliseconds that the deadline must not count.
    pub elapsed: Duration,
}

/// The model's reply, or why there is none. The agent's whole process tree is stopped when
/// the reply arrives or the deadline passes, so a slow model never outlives its request.
/// Call it inside a Tokio runtime: stopping the agent runs as a task of its own.
pub async fn request_title(request: TitleRequest) -> Result<TitleReply, ClientError> {
    let started = Instant::now();
    let mut agent = process::spawn_agent(&request.launch, &request.working_directory)?;
    let stdin = agent
        .stdin
        .take()
        .ok_or(ClientError::MissingPipe("stdin"))?;
    let stdout = agent
        .stdout
        .take()
        .ok_or(ClientError::MissingPipe("stdout"))?;
    let (events, mut received) = unbounded_channel();
    if let Some(stderr) = agent.stderr.take() {
        forward_log(stderr, events.clone());
    }
    let shared = Arc::new(Shared::new(request.working_directory.clone(), events));
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let deadline = request.deadline;
    let session = handlers::register(Client.v1(), &shared)
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            title_turn(&connection, &request).await
        });
    let outcome = tokio::time::timeout(deadline, session).await;
    let elapsed = started.elapsed();
    // Ending an agent's process tree can take seconds (Claude Code's took five), and the
    // thread's setup waits for its title, so the agent is stopped without waiting for it.
    // If the runtime shuts down first, `kill_on_drop` still ends the shim.
    tokio::spawn(async move { process::stop_agent(&mut agent).await });
    outcome.map_err(|_| ClientError::TimedOut(deadline))??;
    Ok(TitleReply {
        text: agent_reply(&mut received),
        elapsed,
    })
}

/// `initialize` → `session/new` → the title model's config → one prompt. The agent sends
/// every `session/update` of the turn before it answers the prompt.
async fn title_turn(
    connection: &ConnectionTo<Agent>,
    request: &TitleRequest,
) -> Result<(), agent_client_protocol::Error> {
    connection
        .send_request(session::initialize_request())
        .block_task()
        .await?;
    let session = connection
        .send_request(NewSessionRequest::new(request.working_directory.clone()))
        .block_task()
        .await?;
    let options = session.config_options.unwrap_or_default();
    apply_settings(connection, &session.session_id, options, &request.settings).await?;
    let prompt = vec![ContentBlock::from(request.prompt.clone())];
    connection
        .send_request(PromptRequest::new(session.session_id, prompt))
        .block_task()
        .await?;
    Ok(())
}

/// Sets one option at a time: choosing a model can change which efforts and speeds the
/// agent offers, so each answer's options decide the next change.
async fn apply_settings(
    connection: &ConnectionTo<Agent>,
    session_id: &SessionId,
    mut options: Vec<SessionConfigOption>,
    settings: &RunSettings<String>,
) -> Result<(), agent_client_protocol::Error> {
    // Each change settles one option, so there are never more rounds than options.
    for _ in 0..=options.len() {
        let views: Vec<ConfigOption<Arc<str>>> = options.iter().filter_map(option_view).collect();
        let chosen = choose_config_values(&views, &settings.model, settings.effort, settings.speed);
        let Some((id, value)) = chosen.into_iter().next() else {
            return Ok(());
        };
        let value = match value {
            ConfigValue::Select(value) => {
                SessionConfigOptionValue::value_id(SessionConfigValueId::new(value))
            }
            ConfigValue::Boolean(value) => SessionConfigOptionValue::boolean(value),
        };
        let request =
            SetSessionConfigOptionRequest::new(session_id.clone(), SessionConfigId::new(id), value);
        options = connection
            .send_request(request)
            .block_task()
            .await?
            .config_options;
    }
    Ok(())
}

/// The pure view of an ACP option. Kinds this build doesn't know are skipped.
fn option_view(option: &SessionConfigOption) -> Option<ConfigOption<Arc<str>>> {
    let category = match option.category {
        Some(SessionConfigOptionCategory::Model) => ConfigCategory::Model,
        Some(SessionConfigOptionCategory::ThoughtLevel) => ConfigCategory::ThoughtLevel,
        _ => ConfigCategory::Other,
    };
    let choices = match &option.kind {
        SessionConfigKind::Select(select) => ConfigChoices::Select {
            current: Arc::clone(&select.current_value.0),
            values: select_values(&select.options),
        },
        SessionConfigKind::Boolean(boolean) => ConfigChoices::Boolean {
            current: boolean.current_value,
        },
        _ => return None,
    };
    Some(ConfigOption::new(
        Arc::clone(&option.id.0),
        Arc::from(option.name.as_str()),
        category,
        choices,
    ))
}

fn select_values(options: &SessionConfigSelectOptions) -> Vec<(Arc<str>, Arc<str>)> {
    let pair = |value: &agent_client_protocol::v1::schema::SessionConfigSelectOption| {
        (Arc::clone(&value.value.0), Arc::from(value.name.as_str()))
    };
    match options {
        SessionConfigSelectOptions::Ungrouped(values) => values.iter().map(pair).collect(),
        SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().map(pair))
            .collect(),
        _ => Vec::new(),
    }
}

/// The text of every agent message chunk the session reported, in order.
fn agent_reply(received: &mut UnboundedReceiver<AgentEvent>) -> String {
    let mut reply = String::new();
    while let Ok(event) = received.try_recv() {
        if let AgentEvent::Transcript {
            update: SessionUpdate::AgentMessageChunk(chunk),
            ..
        } = event
            && let ContentBlock::Text(text) = chunk.content
        {
            reply.push_str(&text.text);
        }
    }
    reply
}
