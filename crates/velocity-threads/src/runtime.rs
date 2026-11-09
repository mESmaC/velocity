//! Live agent sessions by thread (SPEC.md §2.2, §3.2, F4, Rule 1).
//!
//! Each thread's agent runs on its own OS thread with a single-threaded Tokio runtime, the
//! way `velocity_acp::client::run_session` expects: one process per thread, in its worktree.
//! The registry keeps what the thread view shows while the session runs and turns each
//! agent event into the activity `state.json` records.

use std::collections::{BTreeMap, VecDeque};

use agent_client_protocol::v1::schema::ContentBlock;
use thiserror::Error;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;
use velocity_acp::client::{AgentEvent, AgentInput, AgentLaunch, SessionSetup, run_session};
use velocity_acp::{Steering, SteeringEffect, SteeringInput, available_commands};
use velocity_core::thread_view_model::{AgentCommandView, LiveThreadView};
use velocity_store::AgentActivity;

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

struct ThreadRuntime {
    transcript: LiveTranscript,
    inputs: UnboundedSender<AgentInput>,
    /// Mirrors the client's steering (Rule 1): whether a turn is running, and which
    /// messages wait for it to end. The client sees the same sends and turn ends, so the
    /// mirror knows exactly which queued messages each prompt carries.
    steering: Steering<String, ()>,
    log_tail: VecDeque<String>,
    agent_commands: Option<Vec<AgentCommandView>>,
}

#[derive(Default)]
pub struct ThreadRuntimes {
    threads: BTreeMap<ThreadKey, ThreadRuntime>,
}

impl ThreadRuntimes {
    /// Registers a starting thread with its first prompt already waiting, and returns the
    /// inputs its session reads once the worktree is ready. Messages sent meanwhile queue
    /// behind the first prompt in order.
    pub fn start(
        &mut self,
        key: ThreadKey,
        worktree: &str,
        prompt: &str,
    ) -> UnboundedReceiver<AgentInput> {
        let (inputs, receiver) = unbounded_channel();
        // The receiver is alive: it is returned below.
        let _ = inputs.send(text_input(prompt));
        let mut steering = Steering::new(());
        // An idle client prompts the first message at once, so it is never queued.
        steering.step(SteeringInput::Send(prompt.to_owned()));
        self.threads.insert(
            key,
            ThreadRuntime {
                transcript: LiveTranscript::new(worktree),
                inputs,
                steering,
                log_tail: VecDeque::new(),
                agent_commands: None,
            },
        );
        receiver
    }

    /// A message for the thread's agent: sent now when it is idle, queued otherwise (Rule 1).
    pub fn steer(&mut self, key: ThreadKey, prompt: &str) -> Result<(), SteerError> {
        let thread = self.threads.get_mut(&key).ok_or(SteerError::NoSession)?;
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

    pub fn worktree_ready(&mut self, key: ThreadKey) {
        if let Some(thread) = self.threads.get_mut(&key) {
            thread.transcript.worktree_ready();
        }
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
            // The session is over: nothing runs and nothing queued will be sent.
            thread.steering = Steering::new(());
        }
    }

    pub fn apply(&mut self, key: ThreadKey, event: &AgentEvent) -> Option<ThreadProgress> {
        let thread = self.threads.get_mut(&key)?;
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
        Some(LiveThreadView {
            blocks: thread.transcript.blocks(),
            agent_commands: thread.agent_commands.clone(),
        })
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

fn text_input(prompt: &str) -> AgentInput {
    AgentInput::Send(Vec::from([ContentBlock::from(prompt.to_owned())]))
}

/// A session running on its own OS thread.
pub struct RunningSession {
    /// Every agent event, in order. Closes when the session is over.
    pub events: UnboundedReceiver<AgentEvent>,
    /// How the session ended: `Err` holds why it could not start or stopped.
    pub outcome: oneshot::Receiver<Result<(), String>>,
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
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Could not start the agent's runtime: {error}"))
            .and_then(|runtime| {
                runtime
                    .block_on(run_session(agent, setup, inputs, events))
                    .map_err(|error| error.to_string())
            });
        // Nobody waits when the app is closing.
        let _ = finished.send(result);
    });
    RunningSession {
        events: event_receiver,
        outcome,
    }
}
