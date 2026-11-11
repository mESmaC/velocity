//! Live agent sessions by thread (SPEC.md §2.2, §3.2, F4, Rule 1).
//!
//! Each thread's agent runs on its own OS thread with a single-threaded Tokio runtime, the
//! way `velocity_acp::client::run_session` expects: one process per thread, in its worktree.
//! The registry keeps what the thread view shows while the session runs and turns each
//! agent event into the activity `state.json` records.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::v1::schema::ContentBlock;
use thiserror::Error;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use velocity_acp::client::{
    AgentEvent, AgentInput, AgentLaunch, ClientError, SessionSetup, run_session,
};
use velocity_acp::{Steering, SteeringEffect, SteeringInput, available_commands};
use velocity_core::thread_view_model::{AgentCommandView, LiveThreadView, SessionWork};
use velocity_store::AgentActivity;

use crate::follow_through::{Deliveries, DeliveryStatus};
use crate::transcript_view::LiveTranscript;

/// `(project, thread)`: thread identifiers are unique within a project.
pub type ThreadKey = (u32, u32);

/// What a thread sends its agent, read by the session once it starts.
pub type SessionInputs = UnboundedReceiver<AgentInput>;

/// Agent stderr lines kept for the error card when a session fails.
const LOG_TAIL_LINES: usize = 20;

#[derive(Debug, Error)]
pub enum SteerError {
    #[error("No agent session is running for this thread")]
    NoSession,
}

/// What the thread's saved record should show after an agent event (§2.2, F4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadProgress {
    pub activity: AgentActivity,
    pub added: u32,
    pub deleted: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    SettingUp,
    Running,
    Ended,
}

/// Shared with explicit deletion: callers await the session's actual end.
#[derive(Debug)]
pub enum StopOutcome {
    NotRunning,
    SettingUp,
    Stopping(oneshot::Receiver<()>),
}

struct ThreadRuntime {
    phase: SessionPhase,
    session_identifier: String,
    follows_pull_request: bool,
    deliveries: Deliveries,
    transcript: LiveTranscript,
    inputs: UnboundedSender<AgentInput>,
    /// Mirrors the client's steering (Rule 1): whether a turn is running, and which
    /// messages wait for it to end. The client sees the same sends and turn ends, so the
    /// mirror knows exactly which queued messages each prompt carries.
    steering: Steering<String, ()>,
    log_tail: VecDeque<String>,
    agent_commands: Option<Vec<AgentCommandView>>,
    /// The title model's name until the thread's title settles (F4).
    titling_model: Option<String>,
    heartbeat: crate::agent_activity::Heartbeat,
}

#[derive(Default)]
pub struct ThreadRuntimes {
    threads: BTreeMap<ThreadKey, ThreadRuntime>,
    stopping: BTreeMap<ThreadKey, Vec<oneshot::Sender<()>>>,
    retired: BTreeSet<ThreadKey>,
    shutdown_failures: BTreeSet<ThreadKey>,
    /// Threads whose session was ended for parking and not yet resumed (§2.2).
    parked: BTreeSet<ThreadKey>,
    actions: BTreeMap<ThreadKey, Arc<tokio::sync::Mutex<()>>>,
}

impl ThreadRuntimes {
    pub fn heartbeat(&self, key: ThreadKey) -> Option<u64> {
        self.threads.get(&key)?.heartbeat.last_update()
    }

    pub fn observe_supervision(&mut self, key: ThreadKey, batch: &[AgentEvent], now: u64) -> bool {
        self.threads
            .get_mut(&key)
            .is_some_and(|thread| thread.heartbeat.observe(batch, now))
    }

    pub fn nudge_supervision(&mut self, key: ThreadKey, now: u64) {
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.heartbeat.nudge(now);
        }
    }

    /// Merge cleanup and explicit deletion share one lease through their saved checkpoint.
    pub fn action_lock(&mut self, key: ThreadKey) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(self.actions.entry(key).or_default())
    }

    /// Registers a goal swarm member in its choosing work phase before an ACP session
    /// starts (SPEC.md F16). Steering messages sent meanwhile queue behind the claimed
    /// piece's opening prompt once [Self::begin_claimed_session] runs.
    pub fn start_choosing(&mut self, key: ThreadKey, worktree: &str) {
        self.retired.remove(&key);
        self.shutdown_failures.remove(&key);
        let (inputs, _unused) = unbounded_channel();
        let mut steering = Steering::new(());
        steering.step(SteeringInput::Send(String::new()));
        self.threads.insert(
            key,
            ThreadRuntime {
                phase: SessionPhase::SettingUp,
                session_identifier: uuid::Uuid::new_v4().to_string(),
                follows_pull_request: false,
                deliveries: Deliveries::default(),
                transcript: LiveTranscript::new(worktree),
                inputs,
                steering,
                log_tail: VecDeque::new(),
                agent_commands: None,
                titling_model: None,
                heartbeat: crate::agent_activity::Heartbeat::default(),
            },
        );
    }

    /// Transitions a choosing goal swarm member to a live ACP session with opening_prompt
    /// first in its input stream, or returns None if the thread was deleted meanwhile.
    pub fn begin_claimed_session(
        &mut self,
        key: ThreadKey,
        worktree: &str,
        opening_prompt: &str,
    ) -> Option<SessionInputs> {
        let thread = self.threads.get_mut(&key)?;
        if self.retired.contains(&key) || thread.phase == SessionPhase::Ended {
            return None;
        }
        let (inputs, receiver) = unbounded_channel();
        let _ = inputs.send(text_input(opening_prompt));
        if let SteeringEffect::Prompt(queued) = thread.steering.step(SteeringInput::TurnEnded) {
            for message in queued {
                let _ = inputs.send(text_input(&message));
            }
        } else {
            thread.steering = Steering::new(());
            thread
                .steering
                .step(SteeringInput::Send(opening_prompt.to_owned()));
        }
        thread.inputs = inputs;
        thread.phase = SessionPhase::Running;
        thread.transcript.rename_worktree(worktree);
        thread.transcript.worktree_ready();
        Some(receiver)
    }

    /// Drops a choosing runtime entry when an agent leaves without starting a session (F16).
    pub fn discard_choosing(&mut self, key: ThreadKey) {
        self.threads.remove(&key);
        self.stopping.remove(&key);
    }

    /// Registers a starting thread with its first prompt already waiting, and returns the
    /// inputs its session reads once the worktree is ready. Messages sent meanwhile queue
    /// behind the first prompt in order.
    pub fn start(
        &mut self,
        key: ThreadKey,
        worktree: &str,
        prompt: &str,
    ) -> UnboundedReceiver<AgentInput> {
        self.retired.remove(&key);
        self.shutdown_failures.remove(&key);
        let (inputs, receiver) = unbounded_channel();
        // The receiver is alive: it is returned below.
        let _ = inputs.send(text_input(prompt));
        let mut steering = Steering::new(());
        // An idle client prompts the first message at once, so it is never queued.
        steering.step(SteeringInput::Send(prompt.to_owned()));
        self.threads.insert(
            key,
            ThreadRuntime {
                phase: SessionPhase::SettingUp,
                session_identifier: uuid::Uuid::new_v4().to_string(),
                follows_pull_request: false,
                deliveries: Deliveries::default(),
                transcript: LiveTranscript::new(worktree),
                inputs,
                steering,
                log_tail: VecDeque::new(),
                agent_commands: None,
                titling_model: None,
                heartbeat: crate::agent_activity::Heartbeat::default(),
            },
        );
        receiver
    }

    /// Ends a running session to park the thread (SPEC.md §2.2). Unlike [Self::stop], the
    /// thread is not retired and keeps its transcript and queued messages, so
    /// [Self::resume_parked] can start its next session. `None` when no session is running.
    pub fn park(&mut self, key: ThreadKey) -> Option<oneshot::Receiver<()>> {
        if self.retired.contains(&key) || self.stopping.contains_key(&key) {
            return None;
        }
        let thread = self.threads.get_mut(&key)?;
        if thread.phase != SessionPhase::Running {
            return None;
        }
        let _ = thread.inputs.send(AgentInput::Stop);
        thread.phase = SessionPhase::Ended;
        thread.steering = Steering::new(());
        let (ended, receiver) = oneshot::channel();
        self.stopping.insert(key, Vec::from([ended]));
        self.parked.insert(key);
        Some(receiver)
    }

    /// Recovery nudges must reach a silent busy session, rather than waiting behind it.
    pub fn nudge(&mut self, key: ThreadKey, message: &str) -> Result<(), SteerError> {
        self.steer(key, message)
    }

    pub fn is_parked(&self, key: ThreadKey) -> bool {
        self.parked.contains(&key)
    }

    /// Starts a parked thread's next session: `prompt` first, then the messages that were
    /// queued when it parked, in order. After an app restart there is no live transcript,
    /// so a new one starts in the worktree.
    pub fn resume_parked(
        &mut self,
        key: ThreadKey,
        worktree: &str,
        prompt: &str,
        follows_pull_request: bool,
    ) -> Result<SessionInputs, SteerError> {
        let live = self.is_live(key);
        if live || self.retired.contains(&key) || self.stopping.contains_key(&key) {
            return Err(SteerError::NoSession);
        }
        self.shutdown_failures.remove(&key);
        let (inputs, receiver) = unbounded_channel();
        let thread = self
            .threads
            .entry(key)
            .or_insert_with(|| parked_runtime(worktree, follows_pull_request));
        thread.transcript.new_session();
        let mut steering = Steering::new(());
        for message in
            std::iter::once(prompt).chain(thread.transcript.queued().iter().map(String::as_str))
        {
            // The receiver is alive: it is returned below.
            let _ = inputs.send(text_input(message));
            steering.step(SteeringInput::Send(message.to_owned()));
        }
        thread.inputs = inputs;
        thread.steering = steering;
        thread.phase = SessionPhase::Running;
        thread.log_tail.clear();
        self.parked.remove(&key);
        Ok(receiver)
    }

    /// Resumes a saved PR thread in its existing checkout; no new worktree is created.
    pub fn resume_follow_through(
        &mut self,
        key: ThreadKey,
        worktree: &str,
        identifier: &str,
        message: &str,
    ) -> Result<SessionInputs, SteerError> {
        if self.retired.contains(&key) || self.shutdown_failures.contains(&key) {
            return Err(SteerError::NoSession);
        }
        let receiver = self.start(key, worktree, message);
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.phase = SessionPhase::Running;
            thread.follows_pull_request = true;
            thread.deliveries.stage(identifier, message);
            thread.transcript.worktree_ready();
        }
        Ok(receiver)
    }

    pub fn deliver_follow_up(
        &mut self,
        key: ThreadKey,
        identifier: &str,
        message: &str,
    ) -> Result<DeliveryStatus, SteerError> {
        let thread = self.threads.get_mut(&key).ok_or(SteerError::NoSession)?;
        if let Some(status) = thread.deliveries.status(identifier, message) {
            // A parked thread re-sends what was pending when it resumes.
            let parked = self.parked.contains(&key);
            if status == DeliveryStatus::Pending && thread.inputs.is_closed() && !parked {
                return Err(SteerError::NoSession);
            }
            return Ok(status);
        }
        self.steer(key, message)?;
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.deliveries.stage(identifier, message);
        }
        Ok(DeliveryStatus::Pending)
    }

    pub fn work_snapshot(&self, key: ThreadKey) -> Option<SessionWork> {
        let thread = self.threads.get(&key)?;
        Some(SessionWork::new(
            thread.session_identifier.clone(),
            thread.follows_pull_request,
            thread.transcript.tool_steps(),
        ))
    }

    /// A message for the thread's agent: sent now when it is idle, queued otherwise (Rule 1).
    pub fn steer(&mut self, key: ThreadKey, prompt: &str) -> Result<(), SteerError> {
        let thread = self.threads.get_mut(&key).ok_or(SteerError::NoSession)?;
        // A parked agent gets the message right after its resume prompt (§2.2, Rule 1).
        if self.parked.contains(&key) {
            thread.transcript.queue(prompt);
            return Ok(());
        }
        thread
            .inputs
            .send(text_input(prompt))
            .map_err(|_| SteerError::NoSession)?;
        if let SteeringEffect::Queued = thread.steering.step(SteeringInput::Send(prompt.to_owned()))
        {
            thread.transcript.queue(prompt);
        }
        Ok(())
    }

    /// A model, effort or speed picked from the composer menu (F4): the live session applies it
    /// from its next turn and never interrupts the running one (Rule 1).
    pub fn reconfigure(
        &mut self,
        key: ThreadKey,
        settings: velocity_acp::RunSettings<String>,
    ) -> Result<(), SteerError> {
        let thread = self.threads.get(&key).ok_or(SteerError::NoSession)?;
        thread
            .inputs
            .send(AgentInput::Configure(settings))
            .map_err(|_| SteerError::NoSession)
    }

    /// `Some(model)` while the title model names the thread, `None` once its title settles.
    pub fn set_titling(&mut self, key: ThreadKey, model: Option<String>) {
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.titling_model = model;
        }
    }

    /// The worktree was named from the title before setup (F4), so the transcript shows and
    /// resolves paths under its new folder.
    pub fn rename_worktree(&mut self, key: ThreadKey, worktree: &str) {
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.transcript.rename_worktree(worktree);
        }
    }

    /// False when deletion retired the thread during setup (F12).
    pub fn worktree_ready(&mut self, key: ThreadKey) -> bool {
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.phase = SessionPhase::Running;
            thread.transcript.worktree_ready();
            true
        } else {
            false
        }
    }

    pub fn is_live(&self, key: ThreadKey) -> bool {
        self.threads
            .get(&key)
            .is_some_and(|thread| thread.phase != SessionPhase::Ended)
    }

    pub fn is_setting_up(&self, key: ThreadKey) -> bool {
        self.threads
            .get(&key)
            .is_some_and(|thread| thread.phase == SessionPhase::SettingUp)
    }

    /// A watcher must not recreate a session stopped for merge or explicit deletion.
    pub fn is_retired(&self, key: ThreadKey) -> bool {
        self.retired.contains(&key)
    }

    pub fn shutdown_failed(&self, key: ThreadKey) -> bool {
        self.shutdown_failures.contains(&key)
    }

    pub fn stop(&mut self, key: ThreadKey) -> StopOutcome {
        self.retired.insert(key);
        self.parked.remove(&key);
        if let Some(waiters) = self.stopping.get_mut(&key) {
            let (ended, receiver) = oneshot::channel();
            waiters.push(ended);
            return StopOutcome::Stopping(receiver);
        }
        let Some(thread) = self.threads.remove(&key) else {
            return StopOutcome::NotRunning;
        };
        match thread.phase {
            SessionPhase::Ended => StopOutcome::NotRunning,
            SessionPhase::SettingUp => StopOutcome::SettingUp,
            SessionPhase::Running => {
                let _ = thread.inputs.send(AgentInput::Stop);
                let (ended, receiver) = oneshot::channel();
                self.stopping.insert(key, Vec::from([ended]));
                StopOutcome::Stopping(receiver)
            }
        }
    }

    pub fn session_ended(&mut self, key: ThreadKey) -> bool {
        self.session_ended_with_shutdown(key, true)
    }

    pub fn session_ended_with_shutdown(&mut self, key: ThreadKey, confirmed: bool) -> bool {
        if !confirmed {
            self.shutdown_failures.insert(key);
        }
        if let Some(waiters) = self.stopping.remove(&key) {
            for ended in waiters {
                let _ = ended.send(());
            }
            return false;
        }
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.phase = SessionPhase::Ended;
        }
        self.threads.contains_key(&key)
    }

    /// Shows why the thread stopped, with the agent's last stderr lines when it has any.
    pub fn fail(&mut self, key: ThreadKey, headline: &str, detail: &str) {
        if let Some(thread) = self.threads.get_mut(&key) {
            let mut text = detail.to_owned();
            if !thread.log_tail.is_empty() {
                text.push_str("\n\n");
                text.push_str(&Vec::from(thread.log_tail.clone()).join("\n"));
            }
            thread.transcript.record_error(headline, &text);
            thread.phase = SessionPhase::Ended;
            // The session is over: nothing runs and nothing queued will be sent.
            thread.steering = Steering::new(());
        }
    }

    pub fn apply(&mut self, key: ThreadKey, event: &AgentEvent) -> Option<ThreadProgress> {
        let thread = self.threads.get_mut(&key)?;
        thread.deliveries.apply(event);
        match event {
            AgentEvent::Transcript { placement, update } => {
                thread.transcript.record(*placement, update);
            }
            AgentEvent::SessionUpdate(update) => {
                if let Some(commands) = available_commands(update) {
                    thread.agent_commands = Some(
                        commands
                            .into_iter()
                            .map(|command| AgentCommandView {
                                name: command.name,
                                description: command.description,
                            })
                            .collect(),
                    );
                }
                return None;
            }
            AgentEvent::TurnEnded(outcome) => {
                if let Err(error) = outcome {
                    thread
                        .transcript
                        .record_error("The agent's turn failed", error);
                }
                // The client emits the turn end before it prompts what was queued, so the
                // queued rows leave just before the prompt's own message arrives.
                if let SteeringEffect::Prompt(sent) = thread.steering.step(SteeringInput::TurnEnded)
                {
                    thread.transcript.sent_queued(sent.len());
                }
            }
            AgentEvent::Log(line) => {
                thread.log_tail.push_back(line.clone());
                if thread.log_tail.len() > LOG_TAIL_LINES {
                    thread.log_tail.pop_front();
                }
                return None;
            }
            AgentEvent::SessionStarted { .. }
            | AgentEvent::Queued
            | AgentEvent::FileWritten { .. } => return None,
        }
        Some(progress(thread))
    }

    pub fn view(&self, key: ThreadKey) -> Option<LiveThreadView> {
        let thread = self.threads.get(&key)?;
        Some(LiveThreadView::new(
            thread.transcript.blocks(),
            thread.agent_commands.clone(),
            thread.titling_model.clone(),
        ))
    }
}

fn progress(thread: &ThreadRuntime) -> ThreadProgress {
    // A turn that ended leaves no tool running, whatever status its calls last reported.
    let activity = if !thread.steering.is_busy() {
        AgentActivity::Idle
    } else if thread.transcript.has_running_tool() {
        AgentActivity::Executing
    } else {
        AgentActivity::Thinking
    };
    let (added, deleted) = thread.transcript.line_counts();
    ThreadProgress {
        activity,
        added: u32::try_from(added).unwrap_or(u32::MAX),
        deleted: u32::try_from(deleted).unwrap_or(u32::MAX),
    }
}

/// A parked thread with no live record, after an app restart.
fn parked_runtime(worktree: &str, follows_pull_request: bool) -> ThreadRuntime {
    let (inputs, _closed) = unbounded_channel();
    ThreadRuntime {
        phase: SessionPhase::Ended,
        session_identifier: uuid::Uuid::new_v4().to_string(),
        follows_pull_request,
        deliveries: Deliveries::default(),
        transcript: LiveTranscript::new(worktree),
        inputs,
        steering: Steering::new(()),
        log_tail: VecDeque::new(),
        agent_commands: None,
        titling_model: None,
        heartbeat: crate::agent_activity::Heartbeat::default(),
    }
}

fn text_input(prompt: &str) -> AgentInput {
    AgentInput::Send(Vec::from([ContentBlock::from(prompt.to_owned())]))
}

/// A session running on its own OS thread.
pub struct RunningSession {
    /// Every agent event, in order. Closes when the session is over.
    pub events: UnboundedReceiver<AgentEvent>,
    /// How the session ended: `Err` holds why it could not start or stopped.
    pub outcome: oneshot::Receiver<Result<(), String>>,
    pub shutdown: SessionShutdown,
}

#[derive(Clone, Default)]
pub struct SessionShutdown(Arc<AtomicBool>);

impl SessionShutdown {
    pub fn confirmed(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn confirm(&self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Starts the agent in the worktree and runs its session until the inputs close or the
/// agent exits.
pub fn spawn_session(
    agent: AgentLaunch,
    setup: SessionSetup,
    inputs: UnboundedReceiver<AgentInput>,
) -> RunningSession {
    let (events, event_receiver) = unbounded_channel();
    let (finished, outcome) = oneshot::channel();
    let shutdown = SessionShutdown::default();
    let stopped = shutdown.clone();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let result = match runtime {
            Ok(runtime) => {
                let result = runtime.block_on(run_session(agent, setup, inputs, events));
                if !matches!(&result, Err(ClientError::Shutdown(_))) {
                    stopped.confirm();
                }
                result.map_err(|error| error.to_string())
            }
            Err(error) => {
                stopped.confirm();
                Err(format!("Could not start the agent's runtime: {error}"))
            }
        };
        // Nobody waits when the app is closing.
        let _ = finished.send(result);
    });
    RunningSession {
        events: event_receiver,
        outcome,
        shutdown,
    }
}
