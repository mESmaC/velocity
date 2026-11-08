//! Queued steering (Rule 1, F4, §3.2).
//!
//! A message sent while the agent is busy waits for the turn to end and never interrupts
//! it. Everything queued during a turn goes out together, in order, as the next prompt:
//! sending one message per turn instead would hold later messages back for whole extra
//! turns (`proofs/AgentRunner/Design.lean`, `optimal`).

/// What the runner reacts to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteeringInput<Message, Settings> {
    /// You sent a message to the thread.
    Send(Message),
    /// The agent's turn ended, whatever its stop reason (including `cancelled`).
    TurnEnded,
    /// `Esc`: the only input that cancels a turn.
    Interrupt,
    /// A new model, effort or speed. It applies from the next prompt.
    ChangeSettings(Settings),
}

/// What the ACP client does in response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SteeringEffect<Message> {
    /// One `session/prompt` holding these messages in order, sent with
    /// [`Steering::settings`] as they are after the step.
    Prompt(Vec<Message>),
    /// The message waits: "queued · sends when the current step finishes".
    Queued,
    /// `session/cancel`.
    Cancel,
    Nothing,
}

/// One thread's runner state. There is no state in which the agent waits for you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Steering<Message, Settings> {
    busy: bool,
    queue: Vec<Message>,
    settings: Settings,
}

impl<Message, Settings> Steering<Message, Settings> {
    pub fn new(settings: Settings) -> Self {
        Steering {
            busy: false,
            queue: Vec::new(),
            settings,
        }
    }

    /// Whether a turn is running.
    pub fn is_busy(&self) -> bool {
        self.busy
    }

    /// Messages waiting for the current turn to end, oldest first.
    pub fn queued(&self) -> &[Message] {
        &self.queue
    }

    /// The settings the next prompt is sent with.
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn step(&mut self, input: SteeringInput<Message, Settings>) -> SteeringEffect<Message> {
        match input {
            SteeringInput::Send(message) => self.send(message),
            SteeringInput::TurnEnded => self.end_turn(),
            SteeringInput::Interrupt => self.interrupt(),
            SteeringInput::ChangeSettings(settings) => {
                self.settings = settings;
                SteeringEffect::Nothing
            }
        }
    }

    fn send(&mut self, message: Message) -> SteeringEffect<Message> {
        if self.busy {
            self.queue.push(message);
            SteeringEffect::Queued
        } else {
            self.busy = true;
            SteeringEffect::Prompt(Vec::from([message]))
        }
    }

    fn end_turn(&mut self) -> SteeringEffect<Message> {
        if self.queue.is_empty() {
            self.busy = false;
            SteeringEffect::Nothing
        } else {
            // The agent starts its next turn on everything queued, so it stays busy.
            self.busy = true;
            // `swap` rather than `take`: Aeneas models `mem::swap` but not `mem::take`.
            let mut queued = Vec::new();
            std::mem::swap(&mut self.queue, &mut queued);
            SteeringEffect::Prompt(queued)
        }
    }

    fn interrupt(&mut self) -> SteeringEffect<Message> {
        if self.busy {
            SteeringEffect::Cancel
        } else {
            SteeringEffect::Nothing
        }
    }
}
