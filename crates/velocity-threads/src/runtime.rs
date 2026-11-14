//! Live agent sessions by thread (SPEC.md §2.2, §3.2, F4, Rule 1).
//!
//! Each thread's agent runs on its own OS thread with a single-threaded Tokio runtime, the
//! way `velocity_acp::client::run_session` expects: one process per thread, in its worktree.
//! The registry keeps what the thread view shows while the session runs and turns each
//! agent event into the activity `state.json` records.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate};
use thiserror::Error;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use velocity_acp::available_commands;
use velocity_acp::client::{
    AgentEvent, AgentInput, AgentLaunch, ClientError, SessionSetup, run_session,
};
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
    /// How many tool steps the transcript held when this session started. A resumed PR
    /// session reports only the steps after it, so its follow-through cards are its own
    /// work and not every earlier step again.
    session_tool_start: usize,
    deliveries: Deliveries,
    transcript: LiveTranscript,
    inputs: UnboundedSender<AgentInput>,
    /// Client transcript chunks acknowledge inputs in FIFO order. Opening prompts are
    /// invisible here; steering stays visible until its own acknowledgement arrives.
    pending_inputs: VecDeque<PendingInput>,
    /// Velocity's deliveries for a parked thread, sent after its resume prompt (#231).
    held_notices: Vec<String>,
    busy: bool,
    log_tail: VecDeque<String>,
    agent_commands: Option<Vec<AgentCommandView>>,
    /// The title model's name until the thread's title settles (F4).
    titling_model: Option<String>,
    heartbeat: crate::agent_activity::Heartbeat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingInput {
    Opening,
    /// A message you sent; its queued row leaves when the agent reads it.
    Steering,
    /// A line Velocity delivered (AIRC, PR follow-through): shown as a notice, never queued.
    Notice,
}

#[derive(Default)]
pub struct ThreadRuntimes {
    threads: BTreeMap<ThreadKey, ThreadRuntime>,
    stopping: BTreeMap<ThreadKey, Vec<oneshot::Sender<()>>>,
    /// The session [Self::stop] is ending, by thread. Stopping removes the thread's record,
    /// and the session's event pump still has to report its end to wake the waiters above.
    stopped_sessions: BTreeMap<ThreadKey, String>,
    retired: BTreeSet<ThreadKey>,
    shutdown_failures: BTreeSet<ThreadKey>,
    /// Threads whose session was ended for parking and not yet resumed (§2.2).
    parked: BTreeSet<ThreadKey>,
    actions: BTreeMap<ThreadKey, Arc<tokio::sync::Mutex<()>>>,
    transcript_saves: crate::transcript_saves::TranscriptSaves,
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

    /// Rebuilds conversation views without pretending a process survived the restart.
    pub fn restore(&mut self, state: &velocity_store::PersistedState) {
        for project in &state.projects {
            for thread in &project.threads {
                // A merged or done thread never resumes a session, and its transcript is in
                // the saved state already; a runtime would hold a second copy (#322).
                let finished = matches!(
                    thread.state,
                    velocity_store::ThreadState::Merged | velocity_store::ThreadState::Done
                );
                if thread.interview.is_some() || finished {
                    continue;
                }
                let path = thread.worktree_path.as_deref().unwrap_or_default();
                let mut runtime =
                    parked_runtime(path, thread.state == velocity_store::ThreadState::Review);
                runtime.transcript = LiveTranscript::restore(path, thread.transcript.to_vec());
                let key = (project.id, thread.id);
                self.threads.insert(key, runtime);
                if thread.activity == AgentActivity::Parked {
                    self.parked.insert(key);
                }
            }
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
        self.threads.insert(
            key,
            ThreadRuntime {
                phase: SessionPhase::SettingUp,
                session_identifier: uuid::Uuid::new_v4().to_string(),
                follows_pull_request: false,
                session_tool_start: 0,
                deliveries: Deliveries::default(),
                transcript: LiveTranscript::new(worktree),
                inputs,
                pending_inputs: VecDeque::new(),
                held_notices: Vec::new(),
                busy: true,
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
        thread.pending_inputs = VecDeque::from([PendingInput::Opening]);
        for message in thread.transcript.queued() {
            let _ = inputs.send(text_input(message));
            thread.pending_inputs.push_back(PendingInput::Steering);
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
        self.stopped_sessions.remove(&key);
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
        self.threads.insert(
            key,
            ThreadRuntime {
                phase: SessionPhase::SettingUp,
                session_identifier: uuid::Uuid::new_v4().to_string(),
                follows_pull_request: false,
                session_tool_start: 0,
                deliveries: Deliveries::default(),
                transcript: LiveTranscript::new(worktree),
                inputs,
                pending_inputs: VecDeque::from([PendingInput::Opening]),
                held_notices: Vec::new(),
                busy: true,
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
        thread.busy = false;
        thread.pending_inputs.clear();
        let (ended, receiver) = oneshot::channel();
        self.stopping.insert(key, Vec::from([ended]));
        self.parked.insert(key);
        Some(receiver)
    }

    /// Recovery nudges must reach a silent busy session, rather than waiting behind it.
    pub fn nudge(&mut self, key: ThreadKey, message: &str) -> Result<(), SteerError> {
        self.notify(key, message)
    }

    /// A line from Velocity, not from you: the agent reads it at its next step, and the
    /// thread shows it as a notice once read. It never joins your composer queue, so it
    /// never leaves a row there you cannot clear (#231).
    fn notify(&mut self, key: ThreadKey, message: &str) -> Result<(), SteerError> {
        let parked = self.parked.contains(&key);
        let thread = self.threads.get_mut(&key).ok_or(SteerError::NoSession)?;
        if parked {
            thread.held_notices.push(message.to_owned());
            return Ok(());
        }
        thread
            .inputs
            .send(text_input(message))
            .map_err(|_| SteerError::NoSession)?;
        thread.pending_inputs.push_back(PendingInput::Notice);
        Ok(())
    }

    /// Withdraws a message you queued that the agent has not read yet (F4). `index` counts
    /// the thread's queued rows, oldest first.
    pub fn remove_queued(&mut self, key: ThreadKey, index: usize) -> bool {
        let Some(thread) = self.threads.get_mut(&key) else {
            return false;
        };
        let sent = sent_count(thread);
        thread.transcript.remove_queued(index, sent)
    }

    pub fn is_parked(&self, key: ThreadKey) -> bool {
        self.parked.contains(&key)
    }

    /// Starts an ended thread's next session for a message you sent it (#317). The message
    /// joins the queue before the session starts, so it shows as queued at once, its echo
    /// clears it, and a session that ends before the echo (parked, stalled, stopped while
    /// waiting for a provider slot, failed to load) leaves it queued for the next resume.
    pub fn resume_with_message(
        &mut self,
        key: ThreadKey,
        worktree: &str,
        message: &str,
        follows_pull_request: bool,
    ) -> Result<SessionInputs, SteerError> {
        self.ensure_resumable(key)?;
        self.threads
            .entry(key)
            .or_insert_with(|| parked_runtime(worktree, follows_pull_request))
            .transcript
            .queue(message);
        self.resume_parked(key, worktree, None, follows_pull_request)
    }

    fn ensure_resumable(&self, key: ThreadKey) -> Result<(), SteerError> {
        let live = self.is_live(key);
        let input_open = self
            .threads
            .get(&key)
            .is_some_and(|thread| !thread.inputs.is_closed());
        if live && input_open || self.retired.contains(&key) || self.stopping.contains_key(&key) {
            return Err(SteerError::NoSession);
        }
        Ok(())
    }

    /// Starts a parked thread's next session: `prompt` first when there is one, then the
    /// messages that were queued when it parked, in order. Saved history survives an app
    /// restart. With no `prompt` the queued messages open the session; each stays queued
    /// until the agent echoes it.
    pub fn resume_parked(
        &mut self,
        key: ThreadKey,
        worktree: &str,
        prompt: Option<&str>,
        follows_pull_request: bool,
    ) -> Result<SessionInputs, SteerError> {
        self.ensure_resumable(key)?;
        self.shutdown_failures.remove(&key);
        let (inputs, receiver) = unbounded_channel();
        let thread = self
            .threads
            .entry(key)
            .or_insert_with(|| parked_runtime(worktree, follows_pull_request));
        thread.session_identifier = uuid::Uuid::new_v4().to_string();
        thread.transcript.new_session();
        thread.session_tool_start = thread.transcript.tool_steps().len();
        thread.pending_inputs = prompt.map(|_| PendingInput::Opening).into_iter().collect();
        for message in prompt
            .into_iter()
            .chain(thread.transcript.queued().iter().map(String::as_str))
        {
            // The receiver is alive: it is returned below.
            let _ = inputs.send(text_input(message));
        }
        thread.pending_inputs.extend(
            thread
                .transcript
                .queued()
                .iter()
                .map(|_| PendingInput::Steering),
        );
        for notice in std::mem::take(&mut thread.held_notices) {
            let _ = inputs.send(text_input(&notice));
            thread.pending_inputs.push_back(PendingInput::Notice);
        }
        thread.inputs = inputs;
        thread.busy = true;
        thread.phase = SessionPhase::Running;
        thread.log_tail.clear();
        // Commands belong to the new session; it may advertise a different set or none.
        thread.agent_commands = Some(Vec::new());
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
        let receiver = self.resume_parked(key, worktree, Some(message), true)?;
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
        self.notify(key, message)?;
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
            session_steps(thread),
        ))
    }

    /// A message for the thread's agent: sent now when it is idle, queued otherwise (Rule 1).
    /// A queued message stays here, not in the agent's input, until the turn ends, so you
    /// can still withdraw it ([Self::remove_queued]).
    pub fn steer(&mut self, key: ThreadKey, prompt: &str) -> Result<(), SteerError> {
        let thread = self.threads.get_mut(&key).ok_or(SteerError::NoSession)?;
        // A parked agent gets the message right after its resume prompt (§2.2, Rule 1).
        if self.parked.contains(&key) {
            thread.transcript.queue(prompt);
            return Ok(());
        }
        if thread.busy && thread.phase == SessionPhase::Running {
            if thread.inputs.is_closed() {
                return Err(SteerError::NoSession);
            }
            thread.transcript.queue(prompt);
            return Ok(());
        }
        thread
            .inputs
            .send(text_input(prompt))
            .map_err(|_| SteerError::NoSession)?;
        thread.pending_inputs.push_back(PendingInput::Steering);
        thread.transcript.queue(prompt);
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

    /// The thread's folder is a Rosen workspace for `change` (F5).
    pub fn rosen_workspace_ready(&mut self, key: ThreadKey, change: &str) {
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.transcript.rosen_workspace_ready(change);
        }
    }

    pub fn is_live(&self, key: ThreadKey) -> bool {
        self.threads
            .get(&key)
            .is_some_and(|thread| thread.phase != SessionPhase::Ended)
    }

    /// The thread's current session, or the one [Self::stop] is ending after it removed the
    /// thread's record. A session's pump reports its end only while it is current, and that
    /// report is what wakes merge cleanup and deletion waiting for the agent to stop (#278).
    pub fn session_identifier(&self, key: ThreadKey) -> Option<String> {
        self.threads
            .get(&key)
            .map(|thread| thread.session_identifier.clone())
            .or_else(|| self.stopped_sessions.get(&key).cloned())
    }

    /// Whether the thread's agent is at a step boundary: no session runs, or the running one
    /// finished its turn and has nothing sent to it that it has not started on. Velocity
    /// changes the agent's files only here, never under a running step (Rosen rebases, F5).
    pub fn is_at_step_boundary(&self, key: ThreadKey) -> bool {
        if self.stopping.contains_key(&key) {
            return false;
        }
        self.threads
            .get(&key)
            .is_none_or(|thread| match thread.phase {
                SessionPhase::Ended => true,
                SessionPhase::Running => !thread.busy && thread.pending_inputs.is_empty(),
                SessionPhase::SettingUp => false,
            })
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

    /// A stop was asked for and the agent has not exited yet.
    pub fn is_stopping(&self, key: ThreadKey) -> bool {
        self.stopping.contains_key(&key)
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
                self.stopped_sessions.insert(key, thread.session_identifier);
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
            self.stopped_sessions.remove(&key);
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
            thread.busy = false;
            thread.pending_inputs.clear();
        }
    }

    pub fn apply(&mut self, key: ThreadKey, event: &AgentEvent) -> Option<ThreadProgress> {
        let thread = self.threads.get_mut(&key)?;
        thread.deliveries.apply(event);
        match event {
            AgentEvent::Transcript { placement, update } => {
                if matches!(update, SessionUpdate::UserMessageChunk(_)) {
                    thread.busy = true;
                    match thread.pending_inputs.pop_front() {
                        Some(PendingInput::Steering) => thread.transcript.sent_queued(1),
                        Some(PendingInput::Notice) => {
                            thread.transcript.record_notice(*placement, update);
                            return Some(progress(thread));
                        }
                        Some(PendingInput::Opening) | None => {}
                    }
                }
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
                // A delayed turn end cannot acknowledge inputs sent after that turn.
                // Only the client's prompt chunks above remove queued rows.
                thread.busy = false;
                send_held(thread);
            }
            AgentEvent::Log(line) => {
                thread.log_tail.push_back(line.clone());
                if thread.log_tail.len() > LOG_TAIL_LINES {
                    thread.log_tail.pop_front();
                }
                return None;
            }
            AgentEvent::SessionStarted { .. }
            | AgentEvent::ProviderQueued(..)
            | AgentEvent::Queued
            | AgentEvent::FileWritten { .. } => return None,
        }
        Some(progress(thread))
    }

    /// A streaming thread's live transcript when saving it is due (`transcript_saves`).
    pub fn transcript_to_save(
        &mut self,
        key: ThreadKey,
        now: std::time::Instant,
        turn_over: bool,
    ) -> Option<velocity_core::thread_view_model::SharedBlocks> {
        if !self.threads.contains_key(&key) || !self.transcript_saves.due(key, now, turn_over) {
            return None;
        }
        self.view(key).map(|view| view.blocks)
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
    let activity = if !thread.busy
        && (thread.phase == SessionPhase::Ended || thread.transcript.queued().is_empty())
    {
        AgentActivity::Idle
    } else if thread.busy && thread.transcript.has_running_tool() {
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

/// The steps a session's follow-through cards come from. A PR session numbers its cards
/// from its own first step; the first coding session keeps every step, because
/// follow-through skips the steps made before the PR opened.
fn session_steps(thread: &ThreadRuntime) -> Vec<velocity_core::thread_view_model::ToolStepRow> {
    let mut steps = thread.transcript.tool_steps();
    if thread.follows_pull_request {
        steps.drain(..thread.session_tool_start.min(steps.len()));
    }
    steps
}

/// A parked thread with no live record, after an app restart.
fn parked_runtime(worktree: &str, follows_pull_request: bool) -> ThreadRuntime {
    let (inputs, _closed) = unbounded_channel();
    ThreadRuntime {
        phase: SessionPhase::Ended,
        session_identifier: uuid::Uuid::new_v4().to_string(),
        follows_pull_request,
        session_tool_start: 0,
        deliveries: Deliveries::default(),
        transcript: LiveTranscript::new(worktree),
        inputs,
        pending_inputs: VecDeque::new(),
        held_notices: Vec::new(),
        busy: false,
        log_tail: VecDeque::new(),
        agent_commands: None,
        titling_model: None,
        heartbeat: crate::agent_activity::Heartbeat::default(),
    }
}

/// Your queued messages already handed to the agent: they wait only for its echo.
fn sent_count(thread: &ThreadRuntime) -> usize {
    thread
        .pending_inputs
        .iter()
        .filter(|input| **input == PendingInput::Steering)
        .count()
}

/// The turn ended: the messages you queued while it ran go to the agent now, together,
/// as its next prompt (Rule 1).
fn send_held(thread: &mut ThreadRuntime) {
    if thread.phase != SessionPhase::Running {
        return;
    }
    let sent = sent_count(thread);
    let held = thread
        .transcript
        .queued()
        .iter()
        .skip(sent)
        .map(|message| ContentBlock::from(message.clone()))
        .collect::<Vec<_>>();
    let count = held.len();
    if count == 0 || thread.inputs.send(AgentInput::Send(held)).is_err() {
        return;
    }
    thread
        .pending_inputs
        .extend(std::iter::repeat_n(PendingInput::Steering, count));
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

// #261: failed sessions must retain queued user messages across follow-through restart.
#[cfg(test)]
#[test]
fn failed_session_keeps_queued_messages_on_resume() -> Result<(), SteerError> {
    let mut runtimes = ThreadRuntimes::default();
    let key = (1, 2);
    let opening = runtimes.start(key, "checkout", "start");
    runtimes.steer(key, "keep this message")?;
    let generation = runtimes.session_identifier(key);
    drop(opening);
    runtimes.fail(key, "stopped", "transport failed");
    let mut resumed = runtimes.resume_follow_through(key, "checkout", "review-1", "fix review")?;
    assert_ne!(generation, runtimes.session_identifier(key));
    assert!(resumed.try_recv().is_ok());
    assert!(resumed.try_recv().is_ok());
    assert!(resumed.try_recv().is_err());
    assert!(runtimes.view(key).is_some_and(|view| view.blocks.iter().any(|block|
        matches!(block, velocity_core::thread_view_model::TranscriptBlock::UserMessage { text, queued: true, .. } if text == "keep this message"))));
    Ok(())
}

// #278: merge cleanup waits for the stopped session to end before it deletes the worktree. The
// pump reports that end only for the thread's current session, so stopping must not orphan it.
#[cfg(test)]
#[test]
fn stopped_session_still_wakes_its_waiters() -> Result<(), &'static str> {
    let mut runtimes = ThreadRuntimes::default();
    let key = (1, 2);
    let _inputs = runtimes.start(key, "checkout", "start");
    runtimes.worktree_ready(key);
    let generation = runtimes.session_identifier(key);
    let StopOutcome::Stopping(mut ended) = runtimes.stop(key) else {
        return Err("a running session must stop");
    };
    assert_eq!(runtimes.session_identifier(key), generation);
    assert!(!runtimes.session_ended_with_shutdown(key, true));
    assert!(ended.try_recv().is_ok());
    assert_eq!(runtimes.session_identifier(key), None);
    Ok(())
}

// #317: a message sent to an ended thread must not vanish when the new session ends before
// the agent echoes it; it stays queued and goes out again with the next resume.
#[cfg(test)]
#[test]
fn message_to_ended_thread_survives_a_failed_resume() -> Result<(), SteerError> {
    let mut runtimes = ThreadRuntimes::default();
    let key = (1, 2);
    drop(runtimes.start(key, "checkout", "start"));
    runtimes.session_ended(key);
    let mut first = runtimes.resume_with_message(key, "checkout", "keep this", false)?;
    assert!(first.try_recv().is_ok());
    assert!(first.try_recv().is_err());
    drop(first);
    runtimes.fail(key, "Could not load the session", "load failed");
    let mut second = runtimes.resume_parked(key, "checkout", None, false)?;
    assert!(second.try_recv().is_ok());
    assert!(runtimes.view(key).is_some_and(|view| view.blocks.iter().any(|block|
        matches!(block, velocity_core::thread_view_model::TranscriptBlock::UserMessage { text, queued: true } if text == "keep this"))));
    Ok(())
}
