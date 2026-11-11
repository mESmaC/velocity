//! Asking the title model for a thread's title (F4): one short session on the title model's
//! own agent, run while the thread's worktree is being taken.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    ContentBlock, NewSessionRequest, PromptRequest, SessionUpdate,
};
use agent_client_protocol::{Agent, ByteStreams, Client};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::{
    AgentEvent, AgentLaunch, ClientError, Shared, config, forward_log, handlers, process, session,
};
use crate::RunSettings;

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
    /// From requesting the title to its answer, including provider queue time: stopping
    /// a process tree takes a few hundred milliseconds that the deadline must not count.
    pub elapsed: Duration,
}

/// The model's reply, or why there is none. The agent's whole process tree is stopped when
/// the reply arrives or the deadline passes, so a slow model never outlives its request.
/// Call it inside a Tokio runtime: stopping the agent runs as a task of its own.
pub async fn request_title(request: TitleRequest) -> Result<TitleReply, ClientError> {
    let started = Instant::now();
    let permit = tokio::time::timeout(
        request.deadline,
        crate::concurrency::acquire(request.settings.provider, || {}),
    )
    .await
    .map_err(|_| ClientError::TimedOut(request.deadline))?;
    let mut agent = process::spawn_agent(&request.launch, &request.working_directory)?;
    let stdin = agent
        .stdin()
        .take()
        .ok_or(ClientError::MissingPipe("stdin"))?;
    let stdout = agent
        .stdout()
        .take()
        .ok_or(ClientError::MissingPipe("stdout"))?;
    let (events, mut received) = unbounded_channel();
    if let Some(stderr) = agent.stderr().take() {
        forward_log(stderr, events.clone());
    }
    let shared = Arc::new(Shared::new(request.working_directory.clone(), events));
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let deadline = request.deadline.saturating_sub(started.elapsed());
    let session = handlers::register(Client.v1(), &shared)
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            title_turn(&connection, &request).await
        });
    let outcome = tokio::time::timeout(deadline, session).await;
    let elapsed = started.elapsed();
    // Ending an agent's process tree can take seconds (Claude Code's took five), and the
    // thread's setup waits for its title, so the agent is stopped without waiting for it.
    // If the runtime shuts down first, dropping the job object still ends the tree.
    tokio::spawn(async move {
        // Nothing waits on the title agent any more; a failed stop leaves the job object,
        // which ends the tree when its handle drops.
        let _ = process::stop_agent(&mut agent).await;
        drop(permit);
    });
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
    config::apply_settings(connection, &session.session_id, options, &request.settings).await?;
    let prompt = vec![ContentBlock::from(request.prompt.clone())];
    connection
        .send_request(PromptRequest::new(session.session_id, prompt))
        .block_task()
        .await?;
    Ok(())
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
