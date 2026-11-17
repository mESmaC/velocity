//! Background titles (F4): reuse the title agent and its configured ACP session.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    ContentBlock, NewSessionRequest, PromptRequest, SessionId, SessionUpdate,
};
use agent_client_protocol::{Agent, ByteStreams, Client};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::sync::{Mutex, oneshot};
use tokio::time::{Instant as Deadline, timeout_at};
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
    /// The total time F4 allows for a background title, including queueing.
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

static WARM_TITLE_SESSION: Mutex<Option<TitleSession>> = Mutex::const_new(None);
// Keep the title conversation short so each request stays inexpensive.
const TITLES_PER_SESSION: usize = 16;

/// The model's reply, or why there is none. Titles have their own serial queue so they
/// cannot take a provider slot from a thread that's starting.
pub async fn request_title(request: TitleRequest) -> Result<TitleReply, ClientError> {
    let started = Instant::now();
    let duration = request.deadline;
    let deadline = Deadline::now() + duration;
    let expired = |_| ClientError::TimedOut(duration);
    let mut warm = timeout_at(deadline, WARM_TITLE_SESSION.lock())
        .await
        .map_err(expired)?;
    // Take ownership for the turn so a timeout, cancellation or provider error drops the
    // session. A later request never inherits a turn that might still be running.
    let reusable = warm.take().filter(|session| session.matches(&request));
    let (session, text) = timeout_at(deadline, async {
        let mut session = match reusable {
            Some(session) => session,
            None => TitleSession::start(&request).await?,
        };
        let text = session.prompt(request.prompt).await?;
        Ok::<_, ClientError>((session, text))
    })
    .await
    .map_err(expired)??;
    *warm = Some(session);
    Ok(TitleReply {
        text,
        elapsed: started.elapsed(),
    })
}

struct TitleSession {
    launch: AgentLaunch,
    working_directory: PathBuf,
    settings: RunSettings<String>,
    connection: ConnectionTo<Agent>,
    session_id: SessionId,
    received: UnboundedReceiver<AgentEvent>,
    remaining: usize,
    // Dropping the cache entry tells the task to shut down the process tree.
    _stop: oneshot::Sender<()>,
}

impl TitleSession {
    async fn start(request: &TitleRequest) -> Result<Self, ClientError> {
        let (events, received) = unbounded_channel();
        let shared = Arc::new(Shared::new(request.working_directory.clone(), events));
        let (stop, stopped) = oneshot::channel();
        let connection = start_connection(request, shared, stopped).await?;
        let session_id = open_session(&connection, request).await?;
        Ok(Self {
            launch: request.launch.clone(),
            working_directory: request.working_directory.clone(),
            settings: request.settings.clone(),
            connection,
            session_id,
            received,
            remaining: TITLES_PER_SESSION,
            _stop: stop,
        })
    }

    fn matches(&self, request: &TitleRequest) -> bool {
        self.remaining > 0
            && self.launch == request.launch
            && self.working_directory == request.working_directory
            && self.settings == request.settings
            && !self.connection.is_incoming_closed()
            && self.connection.termination().is_none()
    }

    async fn prompt(&mut self, prompt: String) -> Result<String, ClientError> {
        while self.received.try_recv().is_ok() {}
        let prompt = PromptRequest::new(self.session_id.clone(), vec![ContentBlock::from(prompt)]);
        let (send, reply) = oneshot::channel();
        // The ordered callback runs after the message chunk notifications for this turn.
        self.connection
            .send_request(prompt)
            .on_receiving_result(move |result| async move {
                let _ = send.send(result);
                Ok(())
            })?;
        reply
            .await
            .map_err(|_| ClientError::TitleAgentDisconnected)??;
        self.remaining -= 1;
        Ok(agent_reply(&mut self.received))
    }
}

async fn start_connection(
    request: &TitleRequest,
    shared: Arc<Shared>,
    stopped: oneshot::Receiver<()>,
) -> Result<ConnectionTo<Agent>, ClientError> {
    let mut agent = process::spawn_agent(&request.launch, &request.working_directory)?;
    let stdin = agent
        .stdin()
        .take()
        .ok_or(ClientError::MissingPipe("stdin"))?;
    let stdout = agent
        .stdout()
        .take()
        .ok_or(ClientError::MissingPipe("stdout"))?;
    if let Some(stderr) = agent.stderr().take() {
        forward_log(stderr, shared.events.clone());
    }
    let (connected, connection) = oneshot::channel();
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    tokio::spawn(async move {
        let session = handlers::register(Client.v1(), &shared).connect_with(
            transport,
            async move |connection: ConnectionTo<Agent>| {
                if connected.send(connection.clone()).is_ok() {
                    connection.incoming_closed().await;
                }
                Ok(())
            },
        );
        tokio::select! {
            _ = session => {},
            _ = stopped => {},
        }
        let _ = shared.terminals.stop_all().await;
        let _ = process::stop_agent(&mut agent).await;
    });
    connection
        .await
        .map_err(|_| ClientError::TitleAgentDisconnected)
}

async fn open_session(
    connection: &ConnectionTo<Agent>,
    request: &TitleRequest,
) -> Result<SessionId, ClientError> {
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
    Ok(session.session_id)
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
