//! The ACP client edge (SPEC.md §3.2): the agent process, the JSON-RPC connection, and the
//! `fs` and `terminal` capabilities. Every decision it makes comes from the pure
//! modules: which servers and mode a session starts with (`session_start`), how permission
//! requests are answered (`permissions`), when prompts are sent (`steering`), where paths point
//! (`paths`), how much terminal output is kept (`terminal_output`) and where an update goes in
//! the transcript (`transcript`).

mod airc;
mod antigravity_limits;
mod config;
mod coordination;
mod error;
mod files;
mod handlers;
mod handoff;
mod install;
mod kimi_limits;
mod limits;
mod live_limits;
mod own_config;
mod probe;
mod process;
mod program_path;
mod provider_usage_log;
mod queued_run_command;
mod queued_run_tool;
mod queued_runs;
pub mod rewrite;
mod run_queue;
mod session;
mod shared;
mod sign_in;
mod terminals;
mod title;
mod token_usage;
mod token_usage_context;
mod turn_usage;
mod updates;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use agent_client_protocol::v1::ConnectionTo;
use agent_client_protocol::v1::schema::{
    CancelNotification, ContentBlock, ContentChunk, McpServer, PromptRequest, SessionId,
    SessionUpdate, StopReason,
};
use agent_client_protocol::{Agent, ByteStreams, Client};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::ChildStderr;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub use airc::{AircLaunch, duty_member};
pub use antigravity_limits::{
    antigravity_profile_identity, antigravity_profile_signed_in, read_antigravity_usage,
};
pub use error::ClientError;
pub use files::resolve_in_worktree;
pub use handoff::{Decision as HandoffDecision, Experiment as HandoffExperiment};
pub use install::install_agent;
pub use kimi_limits::read_kimi_account;
pub use limits::{
    read_account_label, read_claude_account_usage, read_claude_usage, read_codex_account_usage,
    read_codex_usage,
};
pub use live_limits::{subscribe_account_limits, subscribe_limits, subscription_limit_error};
pub use probe::{AgentProbe, ProbedModel, probe_agent};
pub use process::{
    AgentLaunch, AgentProcess, hidden_command, spawn_owned_command, stop_owned_process,
};
pub use program_path::program_found;
pub use queued_run_command::is_queued_run_notice;
pub use queued_run_tool::{
    QUEUED_RUN_PORT_ENV, QUEUED_RUN_SERVER, QUEUED_RUN_SWITCH, QUEUED_RUN_TOKEN_ENV,
    QUEUED_RUN_TOOL, queued_run_tool, request_queued_run,
};
pub use queued_runs::enable_queued_runs;
pub use session::{
    SessionResume, SessionSetup, StartedSession, answer_permission, session_servers,
};
pub use shared::{SharedServer, install_shared_servers, install_skill_prompt_resolver};
pub use sign_in::sign_in;
pub use title::{TitleReply, TitleRequest, request_title};
pub use updates::{AgentSlashCommand, available_commands, entry_class};

use crate::{Placement, RunSettings, Steering, SteeringEffect, SteeringInput, Transcript};
use queued_runs::{RunQueue, RunResults};
use terminals::Terminals;

/// What the thread sends its agent.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentInput {
    /// A message: prompted now when the agent is idle, queued until the turn ends otherwise
    /// (Rule 1).
    Send(Vec<ContentBlock>),
    /// Coordination joins the next prompt. Only actionable notices can start one.
    Coordination(velocity_airc::Delivery),
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
    /// An update a subagent made, recorded in that subagent's own transcript rather than
    /// the parent's (§3.2 Subagents). `spawn` is the parent's tool call that started it.
    Subagent {
        spawn: Arc<str>,
        placement: Placement,
        update: SessionUpdate,
    },
    /// The grouped AIRC text in this prompt, with all original delivery identifiers.
    CoordinationPrompt {
        identifiers: Vec<String>,
        actions: usize,
        placement: Placement,
        update: SessionUpdate,
    },
    /// An update that is session state rather than transcript (see [`entry_class`]).
    SessionUpdate(SessionUpdate),
    /// `true` before `session/load` replays a history the thread does not hold yet, `false`
    /// once it is over. The user messages in between are history, not echoes of what the
    /// thread just sent (#340).
    HistoryReplay(bool),
    /// A message waits: "queued · sends when the current step finishes".
    Queued,
    /// Waiting for, or admitted to, a provider concurrency slot (F15).
    ProviderQueued(bool, String),
    /// A turn ended: why, or the error the prompt failed with.
    TurnEnded(Result<StopReason, String>),
    /// A successful turn crossed the experiment's context threshold.
    HandoffThreshold {
        change: String,
        decision: Option<HandoffDecision>,
    },
    /// A complete hand-off note, produced at the end of the old session.
    HandoffReady {
        change: String,
        note: String,
        decision: HandoffDecision,
    },
    /// The agent wrote a file through `fs/write_text_file` (for the diff view and claims).
    FileWritten { path: PathBuf },
    /// A session diagnostic from the agent's stderr or the ACP client.
    Log(String),
}

/// State the connection's handlers share.
struct Shared {
    worktree: PathBuf,
    terminals: Terminals,
    transcript: Mutex<Transcript<Arc<str>, SessionUpdate>>,
    subagents: Mutex<crate::SubagentRouter>,
    events: UnboundedSender<AgentEvent>,
    /// Set while `session/load` replays history the thread's transcript already shows.
    replaying: AtomicBool,
    mode_selection: Mutex<Option<session::ModeSelection>>,
    provider: Option<crate::Provider>,
    account: u32,
    code_reads: Mutex<turn_usage::CodeReads>,
    token_capture: Mutex<token_usage::Capture>,
    handoff_text: Mutex<Option<String>>,
}

impl Shared {
    /// Handler state for a session rooted at `worktree`, reporting to `events`.
    fn new(worktree: PathBuf, events: UnboundedSender<AgentEvent>) -> Self {
        Shared {
            terminals: Terminals::new(worktree.clone()),
            worktree,
            transcript: Mutex::new(Transcript::new()),
            subagents: Mutex::default(),
            events,
            replaying: AtomicBool::new(false),
            mode_selection: Mutex::default(),
            provider: None,
            account: 0,
            code_reads: Mutex::default(),
            token_capture: Mutex::default(),
            handoff_text: Mutex::default(),
        }
    }

    fn emit(&self, event: AgentEvent) {
        // The thread stopped listening: the session is ending, nothing to report to.
        let _ = self.events.send(event);
    }

    fn record(&self, update: SessionUpdate) {
        self.record_with(update, |placement, update| AgentEvent::Transcript {
            placement,
            update,
        });
    }

    fn record_with(
        &self,
        update: SessionUpdate,
        event: impl FnOnce(Placement, SessionUpdate) -> AgentEvent,
    ) {
        live_limits::observe_account(self.account, &update);
        if !self.replaying.load(Ordering::Acquire) {
            self.token_capture
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .observe(&update);
        }
        // Before the hand-off capture: a subagent's text is never the parent's note.
        if self.record_subagent(&update) {
            return;
        }
        if handoff::capture(self, &update) {
            return;
        }
        self.code_reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(&update);
        match entry_class(&update) {
            Some(_) if self.replaying.load(Ordering::Acquire) => {}
            Some(class) => {
                let placement = self
                    .transcript
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .record(class, update.clone());
                self.emit(event(placement, update));
            }
            None => self.emit(AgentEvent::SessionUpdate(update)),
        }
    }

    /// Files a subagent's update in that subagent's transcript and reports it. Returns
    /// `false` for the parent's own updates. Replayed history is skipped, as the parent's
    /// is, so the thread's copy of each subagent transcript keeps ACP's indices.
    fn record_subagent(&self, update: &SessionUpdate) -> bool {
        let Some(class) = entry_class(update) else {
            return false;
        };
        if self.replaying.load(Ordering::Acquire) {
            return crate::subagents::attributed_spawn(update).is_some();
        }
        let routed = self
            .subagents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .route(update, &class);
        let Some((spawn, placement)) = routed else {
            return false;
        };
        // A subagent's code reads are still this turn's reads.
        self.code_reads
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(update);
        self.emit(AgentEvent::Subagent {
            spawn,
            placement,
            update: update.clone(),
        });
        true
    }
}

/// The shared servers a session in `worktree` gets (§3.2 Tool access): Settings → Agents' list
/// from the launch, then the app's servers for the folder's project (F18: an always-on pack's
/// MCP server, such as REA's), minus every name the agent's own config already has.
pub fn launch_shared_servers(launch: &AgentLaunch, worktree: &std::path::Path) -> Vec<McpServer> {
    let mut servers = launch.shared_servers.clone();
    servers.extend(shared::shared_servers_for(worktree));
    own_config::without_own_servers(&servers, launch, worktree)
}

/// Runs one thread's agent until the thread stops it or the agent exits: spawns the agent in
/// the worktree, starts the session, then feeds it the thread's messages.
pub async fn run_session(
    launch: AgentLaunch,
    setup: SessionSetup,
    inputs: UnboundedReceiver<AgentInput>,
    events: UnboundedSender<AgentEvent>,
) -> Result<(), ClientError> {
    let (inputs, stopped) = forward_inputs(inputs);
    let provider = launch.concurrency_provider.or_else(|| {
        setup
            .run_settings
            .as_ref()
            .map(|settings| settings.provider)
    });
    let _permit = if let Some(provider) = provider {
        let permit = tokio::select! {
            permit = crate::concurrency::acquire(provider, || {
                let _ = events.send(AgentEvent::ProviderQueued(true, provider.label()));
            }) => permit,
            _ = stopped => return Ok(()),
        };
        let _ = events.send(AgentEvent::ProviderQueued(false, provider.label()));
        Some(permit)
    } else {
        None
    };
    run_admitted_session(launch, setup, inputs, events).await
}

async fn run_admitted_session(
    mut launch: AgentLaunch,
    setup: SessionSetup,
    inputs: UnboundedReceiver<AgentInput>,
    events: UnboundedSender<AgentEvent>,
) -> Result<(), ClientError> {
    let mut setup = setup;
    // Meter availability must never prevent starting an agent.
    let _ = live_limits::prepare(&mut launch).await;
    let (mut queue, results) = match RunQueue::start(&mut setup, &launch).await {
        Ok(Some((queue, results))) => (Some(queue), results),
        unavailable => {
            if let Err(error) = unavailable {
                let _ = events.send(AgentEvent::Log(format!(
                    "Could not start the queued run tool: {error}"
                )));
            }
            setup.preface = run_queue::preface(&launch, setup.preface.take());
            (None, RunResults::default())
        }
    };
    // Velocity's own servers come first in the setup, so a clash keeps them (session_servers).
    setup
        .shared_servers
        .extend(launch_shared_servers(&launch, &setup.worktree));
    if launch.own_config == Some(crate::Provider::Anthropic)
        && let Err(error) = setup.load_claude_project_instructions().await
    {
        let _ = events.send(AgentEvent::Log(format!(
            "Could not load project AGENTS.md: {error}"
        )));
    }
    let mut agent = process::spawn_agent(&launch, &setup.worktree)?;
    let shared = Arc::new(Shared {
        worktree: setup.worktree.clone(),
        terminals: Terminals::new(setup.worktree.clone()),
        transcript: Mutex::new(Transcript::new()),
        subagents: Mutex::default(),
        events,
        replaying: AtomicBool::new(false),
        mode_selection: Mutex::default(),
        provider: launch.concurrency_provider.or(launch.own_config),
        account: launch.account,
        code_reads: Mutex::default(),
        token_capture: Mutex::default(),
        handoff_text: Mutex::default(),
    });
    let result = connect_agent(
        &mut agent,
        Arc::clone(&shared),
        setup,
        launch,
        inputs,
        results,
    )
    .await;
    let queue_stopped = if let Some(queue) = &mut queue {
        queue.stop().await
    } else {
        Ok(())
    };
    let terminals_stopped = shared.terminals.stop_all().await;
    let agent_stopped = process::stop_agent(&mut agent)
        .await
        .map_err(|error| ClientError::Shutdown(error.to_string()));
    terminals_stopped?;
    agent_stopped?;
    queue_stopped.map_err(|error| ClientError::Shutdown(error.to_string()))?;
    result
}

async fn connect_agent(
    agent: &mut process::AgentProcess,
    shared: Arc<Shared>,
    setup: SessionSetup,
    launch: AgentLaunch,
    inputs: UnboundedReceiver<AgentInput>,
    results: RunResults,
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
        forward_account_log(stderr, shared.events.clone(), shared.account);
    }
    let transport = ByteStreams::new(stdin.compat_write(), stdout.compat());
    let session_state = Arc::clone(&shared);
    let (inputs, stopped) = forward_inputs(inputs);
    let connection = handlers::register(Client.v1(), &shared).connect_with(
        transport,
        async move |connection: ConnectionTo<Agent>| {
            drive(connection, session_state, setup, launch, inputs, results).await
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
            let stopping = matches!(input, AgentInput::Stop);
            if forwarded.send(input).is_err() || stopping {
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
    forward_account_log(stderr, events, 0);
}

fn forward_account_log(stderr: ChildStderr, events: UnboundedSender<AgentEvent>, account: u32) {
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
                    if !live_limits::observe_account_log(account, &text) {
                        let _ = events.send(AgentEvent::Log(text));
                    }
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
    launch: AgentLaunch,
    inputs: UnboundedReceiver<AgentInput>,
    runs: RunResults,
) -> Result<(), agent_client_protocol::Error> {
    let session_start = token_usage::now();
    let started = start_reporting_replay(&connection, &shared, &setup).await?;
    let session_id = started.session_id;
    let meter = turn_usage::UsageMeter::for_setup(&setup, started.graph_on);
    let since = if started.loaded { session_start } else { 0 };
    let tokens = token_usage::SessionMeter::new(&setup, &launch, &session_id.0, &shared, since);
    shared
        .code_reads
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .start();
    shared.emit(AgentEvent::SessionStarted {
        session_id: session_id.clone(),
        loaded: started.loaded,
    });
    let mut preface = session_preface(&setup, started.loaded, started.graph_on).await;
    let (turn_sender, turn_ended) = unbounded_channel();
    let mut coordination = coordination::Coordination::default();
    let mut turn = Turn {
        connection: &connection,
        shared: &shared,
        session_id: &session_id,
        turn_sender: &turn_sender,
        preface: &mut preface,
        coordination: &mut coordination,
    };
    turn.drive_turns(
        inputs,
        turn_ended,
        TurnAccounting {
            meter,
            tokens,
            prompt_usage: turn_usage::PromptUsage::for_launch(&launch),
            handoff: setup.handoff.map(handoff::Boundary::new),
        },
        started.options,
        setup.pause_on_subscription_limit,
        runs,
    )
    .await
}

/// The workspace note, any transcript needed for recovery, and a fresh session's graph hints.
async fn session_preface(setup: &SessionSetup, loaded: bool, graph_on: bool) -> Vec<ContentBlock> {
    let graph_preface = if !loaded && graph_on {
        airc::graph_preface(setup).await
    } else {
        None
    };
    setup
        .preface
        .iter()
        .cloned()
        .chain(setup.handoff_context.iter().cloned())
        .chain(
            setup
                .resume
                .iter()
                .filter(|_| !loaded)
                .map(|resume| resume.transcript.clone()),
        )
        .chain(graph_preface)
        .map(|text| {
            ContentBlock::from(velocity_core::text_budget::prompt_excerpt(
                &text,
                velocity_core::text_budget::PROMPT_BYTES,
                "\n[Context excerpt. Use the graph or the workspace files for further detail.]",
            ))
        })
        .collect()
}

struct PromptResult {
    outcome: Result<StopReason, String>,
    usage: Option<agent_client_protocol::v1::schema::Usage>,
    elapsed: std::time::Duration,
}

struct TurnAccounting {
    meter: Option<turn_usage::UsageMeter>,
    tokens: Option<token_usage::SessionMeter>,
    handoff: Option<handoff::Boundary>,
    prompt_usage: turn_usage::PromptUsage,
}

impl TurnAccounting {
    async fn record(&mut self, result: &PromptResult, shared: &Shared) -> Option<u64> {
        let usage = self.prompt_usage.turn(result.usage.as_ref());
        let context = match &mut self.tokens {
            Some(tokens) => tokens.finish(usage).await,
            None => None,
        };
        if let Some(meter) = &self.meter {
            let reads = shared
                .code_reads
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take_count();
            meter
                .record(usage, result.elapsed, reads, |line| {
                    shared.emit(AgentEvent::Log(line))
                })
                .await;
        }
        context
    }
}

/// Starts the session, bracketing it with [`AgentEvent::HistoryReplay`] when a loaded session
/// will replay its history into the transcript. The bracket closes even when the start fails,
/// so the thread never mistakes a later echo for history.
async fn start_reporting_replay(
    connection: &ConnectionTo<Agent>,
    shared: &Shared,
    setup: &SessionSetup,
) -> Result<session::StartedSession, agent_client_protocol::Error> {
    let replays_history = setup
        .resume
        .as_ref()
        .is_some_and(|resume| resume.replay_history);
    if replays_history {
        shared.emit(AgentEvent::HistoryReplay(true));
    }
    let started = session::start_or_resume_session(connection, setup, shared).await;
    if replays_history {
        shared.emit(AgentEvent::HistoryReplay(false));
    }
    started
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
    turn_sender: &'session UnboundedSender<PromptResult>,
    preface: &'session mut Vec<ContentBlock>,
    coordination: &'session mut coordination::Coordination,
}

impl Turn<'_> {
    async fn drive_turns(
        &mut self,
        mut inputs: UnboundedReceiver<AgentInput>,
        mut turn_ended: UnboundedReceiver<PromptResult>,
        mut accounting: TurnAccounting,
        mut options: Vec<agent_client_protocol::v1::schema::SessionConfigOption>,
        pause_on_subscription_limit: bool,
        mut runs: RunResults,
    ) -> Result<(), agent_client_protocol::Error> {
        let mut steering = Steering::new(());
        loop {
            let input = tokio::select! {
                input = inputs.recv() => match input {
                    Some(AgentInput::Send(message)) => SteeringInput::Send(message),
                    Some(AgentInput::Coordination(delivery)) => {
                        self.coordination.push(delivery);
                        continue;
                    }
                    Some(AgentInput::Interrupt) if runs.waiting => continue,
                    Some(AgentInput::Interrupt) => SteeringInput::Interrupt,
                    Some(AgentInput::Configure(settings)) => {
                        options = reconfigure(self.connection, self.shared, self.session_id, options, &settings).await;
                        continue;
                    }
                    Some(AgentInput::Stop) | None => return Ok(()),
                },
                () = self.coordination.wait(), if !steering.is_busy() => SteeringInput::Send(Vec::new()),
                event = runs.receive() => {
                    runs.record(event, self.shared);
                    if runs.waiting && !runs.outstanding() {
                        self.continue_after_runs(&mut steering, &mut runs)?;
                    }
                    continue;
                },
                Some(result) = turn_ended.recv() => {
                    let context = accounting.record(&result, self.shared).await;
                    let outcome = result.outcome;
                    let outcome = match self.handoff_boundary(&mut accounting.handoff, &runs, outcome, context).await? {
                        handoff::TurnBoundary::Finish(outcome) => outcome,
                        handoff::TurnBoundary::Summarizing => continue,
                        handoff::TurnBoundary::Replace(outcome) => {
                            self.shared.emit(AgentEvent::TurnEnded(outcome));
                            return Ok(());
                        }
                    };
                    let paused = pause_on_subscription_limit
                        && outcome.as_ref().is_err_and(|error| subscription_limit_error(error));
                    if !paused && (runs.outstanding() || !runs.ready.is_empty()) {
                        // Keep the logical turn open so duty runners do not stop or nudge
                        // the session while the provider has no prompt in flight.
                        runs.waiting = true;
                        runs.release();
                        if !runs.outstanding() {
                            self.continue_after_runs(&mut steering, &mut runs)?;
                        }
                        continue;
                    }
                    self.shared.emit(AgentEvent::TurnEnded(outcome));
                    if paused {
                        // The saved thread owns prompts it hasn't seen echoed. Its supervisor
                        // stops this process before starting a new login.
                        while let Some(input) = inputs.recv().await {
                            if matches!(input, AgentInput::Stop) {
                                break;
                            }
                        }
                        return Ok(());
                    }
                    self.coordination.turn_ended();
                    SteeringInput::TurnEnded
                }
            };
            self.apply(steering.step(input))?;
        }
    }

    fn continue_after_runs(
        &mut self,
        steering: &mut Steering<Vec<ContentBlock>, ()>,
        runs: &mut RunResults,
    ) -> Result<(), agent_client_protocol::Error> {
        runs.waiting = false;
        for block in &runs.ready {
            self.shared
                .record(queued_run_command::completion_notice(block.clone()));
        }
        self.preface.append(&mut runs.ready);
        let effect = match steering.step(SteeringInput::TurnEnded) {
            SteeringEffect::Nothing => steering.step(SteeringInput::Send(Vec::new())),
            effect => effect,
        };
        self.apply(effect)
    }

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
        if let Some((message, identifiers, actions)) = self.coordination.take_prompt() {
            let block = ContentBlock::from(message);
            self.shared.record_with(
                SessionUpdate::UserMessageChunk(ContentChunk::new(block.clone())),
                |placement, update| AgentEvent::CoordinationPrompt {
                    identifiers,
                    actions,
                    placement,
                    update,
                },
            );
            blocks.push(block);
        }
        let skill_instructions: Vec<_> = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => shared::skill_prompt(&self.shared.worktree, &text.text),
                _ => None,
            })
            .map(ContentBlock::from)
            .collect();
        blocks.extend(skill_instructions);
        // The thread view already shows the transcript, so the preface is sent unrecorded.
        if !self.preface.is_empty() {
            let mut sent = std::mem::take(self.preface);
            sent.append(&mut blocks);
            blocks = sent;
        }
        self.request(blocks)
    }

    fn request(&self, blocks: Vec<ContentBlock>) -> Result<(), agent_client_protocol::Error> {
        self.shared
            .token_capture
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .prompt(&blocks);
        let turn_sender = self.turn_sender.clone();
        let started = std::time::Instant::now();
        self.connection
            .send_request(PromptRequest::new(self.session_id.clone(), blocks))
            .on_receiving_result(move |result| async move {
                let usage = result
                    .as_ref()
                    .ok()
                    .and_then(|response| response.usage.clone());
                let outcome = result
                    .map(|response| response.stop_reason)
                    .map_err(|error| live_limits::turn_error(&error));
                // The loop ended: the session is stopping and no one waits for the turn.
                let _ = turn_sender.send(PromptResult {
                    outcome,
                    usage,
                    elapsed: started.elapsed(),
                });
                Ok(())
            })
    }
}
