//! The ACP client edge (SPEC.md §3.2): the agent process, the JSON-RPC connection, and the
//! `fs` and `terminal` capabilities. Every decision it makes comes from the pure
//! modules: which servers and mode a session starts with (`session_start`), how permission
//! requests are answered (`permissions`), when prompts are sent (`steering`), where paths point
//! (`paths`), how much terminal output is kept (`terminal_output`) and where an update goes in
//! the transcript (`transcript`).

mod airc;
mod config;
mod error;
mod files;
mod handlers;
mod limits;
mod own_config;
mod probe;
mod process;
mod program_path;
pub mod rewrite;
mod session;
mod sign_in;
mod terminals;
mod title;
mod updates;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    CancelNotification, ContentBlock, ContentChunk, PromptRequest, SessionId, SessionUpdate,
    StopReason,
};
use agent_client_protocol::{Agent, ByteStreams, Client};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub use airc::AircLaunch;
pub use error::ClientError;
pub use files::resolve_in_worktree;
pub use limits::{read_claude_usage, read_codex_usage};
pub use probe::{AgentProbe, ProbedModel, probe_agent};
pub use process::AgentLaunch;
pub use session::{
    SessionResume, SessionSetup, StartedSession, answer_permission, session_servers,
};
pub use sign_in::sign_in;
pub use title::{TitleReply, TitleRequest, request_title};
pub use updates::{AgentSlashCommand, available_commands, entry_class};

use crate::{Placement, RunSettings, Steering, SteeringEffect, SteeringInput, Transcript};
use terminals::Terminals;

/// What the thread sends its agent.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentInput {
    /// A message: prompted now when the agent is idle, queued until the turn ends otherwise
    /// (Rule 1).
    Send(Vec<ContentBlock>),
    /// `Esc`: `session/cancel` when a turn is running.
    Interrupt,
    /// A new model, effort or speed chosen from the composer menu (F4, F15). The agent
    /// applies it from its next turn; a running turn is never interrupted (Rule 1).
    Configure(RunSettings<String>),
    /// Ends the session and stops the agent. Dropping the sender does the same.
    Stop,
}

/// What the agent's session reports.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    SessionStarted {
        session_id: SessionId,
        /// True when `session/load` resumed the thread's earlier session (§2.2).
        loaded: bool,
    },
    /// An update recorded in the transcript, and where it went.
    Transcript {
        placement: Placement,
        update: SessionUpdate,
    },
    /// An update that is session state rather than transcript (see [`entry_class`]).
    SessionUpdate(SessionUpdate),
    /// A message waits: "queued · sends when the current step finishes".
    Queued,
    /// A turn ended: why, or the error the prompt failed with.
    TurnEnded(Result<StopReason, String>),
    /// The agent wrote a file through `fs/write_text_file` (for the diff view and claims).
    FileWritten { path: PathBuf },
    /// A line the agent wrote to stderr.
    Log(String),
}

/// State the connection's handlers share.
struct Shared {
    worktree: PathBuf,
    terminals: Terminals,
    transcript: Mutex<Transcript<Arc<str>, SessionUpdate>>,
    events: UnboundedSender<AgentEvent>,
    /// Set while `session/load` replays history the thread's transcript already shows.
    replaying: AtomicBool,
}

impl Shared {
    /// Handler state for a session rooted at `worktree`, reporting to `events`.
    fn new(worktree: PathBuf, events: UnboundedSender<AgentEvent>) -> Self {
        Shared {
            terminals: Terminals::new(worktree.clone()),
            worktree,
            transcript: Mutex::new(Transcript::new()),
            events,
            replaying: AtomicBool::new(false),
        }
    }

    fn emit(&self, event: AgentEvent) {
        // The thread stopped listening: the session is ending, nothing to report to.
        let _ = self.events.send(event);
    }

    fn record(&self, update: SessionUpdate) {
        match entry_class(&update) {
            Some(_) if self.replaying.load(Ordering::Acquire) => {}
            Some(class) => {
                let placement = self
                    .transcript
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .record(class, update.clone());
                self.emit(AgentEvent::Transcript { placement, update });
            }
            None => self.emit(AgentEvent::SessionUpdate(update)),
        }
    }
}

/// Runs one thread's agent until the thread stops it or the agent exits: spawns the agent in
/// the worktree, starts the session, then feeds it the thread's messages.
pub async fn run_session(
    launch: AgentLaunch,
    setup: SessionSetup,
    inputs: UnboundedReceiver<AgentInput>,
    events: UnboundedSender<AgentEvent>,
) -> Result<(), ClientError> {
    let mut setup = setup;
    // Velocity's own servers come first in the setup, so a clash keeps them (session_servers).
    setup.shared_servers.extend(own_config::without_own_servers(
        &launch.shared_servers,
        launch.own_config,
        &setup.worktree,
    ));
    let mut agent = process::spawn_agent(&launch, &setup.worktree)?;
    let shared = Arc::new(Shared {
        worktree: setup.worktree.clone(),
        terminals: Terminals::new(setup.worktree.clone()),
        transcript: Mutex::new(Transcript::new()),
        events,
        replaying: AtomicBool::new(false),
    });
    let result = connect_agent(&mut agent, Arc::clone(&shared), setup, inputs).await;
    let terminals_stopped = shared.terminals.stop_all().await;
    let agent_stopped = process::stop_agent(&mut agent)
        .await
        .map_err(|error| ClientError::Shutdown(error.to_string()));
    terminals_stopped?;
    agent_stopped?;
    result
}

async fn connect_agent(
    agent: &mut process::AgentProcess,
    shared: Arc<Shared>,
    setup: SessionSetup,
    inputs: UnboundedReceiver<AgentInput>,
) -> Result<(), ClientError> {
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
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let session_state = Arc::clone(&shared);
    let (inputs, stopped) = forward_inputs(inputs);
    let connection = handlers::register(Client.v1(), &shared).connect_with(
        transport,
        async move |connection: ConnectionTo<Agent>| {
            drive(connection, session_state, setup, inputs).await
        },
    );
    let result = tokio::select! {
        result = connection => result,
        _ = stopped => return Ok(()),
    };
    result.map_err(ClientError::from)
}

/// Stop also cancels a hung initialize/session-new request, before drive reads prompts.
fn forward_inputs(
    mut inputs: UnboundedReceiver<AgentInput>,
) -> (UnboundedReceiver<AgentInput>, oneshot::Receiver<()>) {
    let (forwarded, receiver) = unbounded_channel();
    let (stop, stopped) = oneshot::channel();
    tokio::spawn(async move {
        while let Some(input) = inputs.recv().await {
            if matches!(input, AgentInput::Stop) || forwarded.send(input).is_err() {
                break;
            }
        }
        let _ = stop.send(());
    });
    (receiver, stopped)
}

/// Reads the agent's stderr to the end, whatever it holds. Stopping early would close the
/// pipe and make the agent's next stderr write fail, which crashes many agents. So lines that
/// aren't UTF-8 (code-page output from git or cmd) are forwarded lossily, and lines are still
/// read and dropped once nobody listens.
fn forward_log(stderr: ChildStderr, events: UnboundedSender<AgentEvent>) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(without_line_end(&line)).into_owned();
                    // No receiver: the session is ending; keep draining regardless.
                    let _ = events.send(AgentEvent::Log(text));
                }
            }
        }
    });
}

fn without_line_end(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// The session's main loop: thread inputs and ended turns go through [`Steering`], whose
/// effects become ACP messages.
async fn drive(
    connection: ConnectionTo<Agent>,
    shared: Arc<Shared>,
    setup: SessionSetup,
    mut inputs: UnboundedReceiver<AgentInput>,
) -> Result<(), agent_client_protocol::Error> {
    let started = session::start_or_resume_session(&connection, &setup, &shared.replaying).await?;
    let session_id = started.session_id;
    let mut options = started.options;
    shared.emit(AgentEvent::SessionStarted {
        session_id: session_id.clone(),
        loaded: started.loaded,
    });
    // A new session for a resumed thread is sent its transcript ahead of the first prompt.
    let mut preface = setup
        .resume
        .filter(|_| !started.loaded)
        .map(|resume| ContentBlock::from(resume.transcript));
    let (turn_sender, mut turn_ended) = unbounded_channel();
    let mut steering: Steering<Vec<ContentBlock>, ()> = Steering::new(());
    loop {
        let input = tokio::select! {
            input = inputs.recv() => match input {
                Some(AgentInput::Send(message)) => SteeringInput::Send(message),
                Some(AgentInput::Interrupt) => SteeringInput::Interrupt,
                Some(AgentInput::Configure(settings)) => {
                    options = reconfigure(&connection, &shared, &session_id, options, &settings).await;
                    continue;
                }
                Some(AgentInput::Stop) | None => return Ok(()),
            },
            Some(outcome) = turn_ended.recv() => {
                shared.emit(AgentEvent::TurnEnded(outcome));
                SteeringInput::TurnEnded
            }
        };
        let mut turn = Turn {
            connection: &connection,
            shared: &shared,
            session_id: &session_id,
            turn_sender: &turn_sender,
            preface: &mut preface,
        };
        turn.apply(steering.step(input))?;
    }
}

/// Applies new run settings mid-session. A refused change is logged and leaves the session on
/// its previous settings rather than ending it.
async fn reconfigure(
    connection: &ConnectionTo<Agent>,
    shared: &Shared,
    session_id: &SessionId,
    options: Vec<agent_client_protocol::v1::schema::SessionConfigOption>,
    settings: &RunSettings<String>,
) -> Vec<agent_client_protocol::v1::schema::SessionConfigOption> {
    match config::apply_settings(connection, session_id, options.clone(), settings).await {
        Ok(updated) => updated,
        Err(error) => {
            shared.emit(AgentEvent::Log(format!(
                "Could not switch to {}: {error}",
                settings.model
            )));
            options
        }
    }
}

/// What applying a steering effect needs.
struct Turn<'session> {
    connection: &'session ConnectionTo<Agent>,
    shared: &'session Shared,
    session_id: &'session SessionId,
    turn_sender: &'session UnboundedSender<Result<StopReason, String>>,
    preface: &'session mut Option<ContentBlock>,
}

impl Turn<'_> {
    fn apply(
        &mut self,
        effect: SteeringEffect<Vec<ContentBlock>>,
    ) -> Result<(), agent_client_protocol::Error> {
        match effect {
            SteeringEffect::Prompt(messages) => self.prompt(messages),
            SteeringEffect::Queued => {
                self.shared.emit(AgentEvent::Queued);
                Ok(())
            }
            SteeringEffect::Cancel => self
                .connection
                .send_notification(CancelNotification::new(self.session_id.clone())),
            SteeringEffect::Nothing => Ok(()),
        }
    }

    /// One `session/prompt` for every message sent together. Agents do not echo the prompt
    /// back, so it is recorded in the transcript here.
    fn prompt(
        &mut self,
        messages: Vec<Vec<ContentBlock>>,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut blocks: Vec<ContentBlock> = messages.into_iter().flatten().collect();
        for block in &blocks {
            self.shared
                .record(SessionUpdate::UserMessageChunk(ContentChunk::new(
                    block.clone(),
                )));
        }
        // The thread view already shows the transcript, so the preface is sent unrecorded.
        if let Some(preface) = self.preface.take() {
            blocks.insert(0, preface);
        }
        let turn_sender = self.turn_sender.clone();
        self.connection
            .send_request(PromptRequest::new(self.session_id.clone(), blocks))
            .on_receiving_result(move |result| async move {
                let outcome = result
                    .map(|response| response.stop_reason)
                    .map_err(|error| error.to_string());
                // The loop ended: the session is stopping and no one waits for the turn.
                let _ = turn_sender.send(outcome);
                Ok(())
            })
    }
}
