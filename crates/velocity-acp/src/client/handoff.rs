//! Summarize at the ACP prompt boundary, before steering can send the next queued input.

use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::oneshot;

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate, StopReason};

use super::{AgentEvent, RunResults, Shared, Turn};

const CONTEXT_THRESHOLD: u64 = 120_000;
const MAX_NOTE_BYTES: usize = velocity_core::text_budget::SHORT_PROMPT_BYTES;
const HEADINGS: [&str; 6] = [
    "Goal",
    "Done",
    "Remaining",
    "Files touched",
    "Decisions",
    "Open failures",
];
const PROMPT: &str = "Velocity is handing this work item to a fresh session. Do not continue the work or use tools. Write a concise hand-off under 3,500 bytes, with each of the following Markdown headings on its own line and useful text below it. Write None for an empty section. Include exact paths, outstanding checks and errors, and the next step where applicable.\n\n## Goal\n## Done\n## Remaining\n## Files touched\n## Decisions\n## Open failures";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Experiment {
    pub change: String,
    pub enabled: bool,
    /// Goal selection runs before the work item has been claimed.
    pub skip_first_turn: bool,
}

impl Experiment {
    pub fn new(change: String, enabled: bool) -> Self {
        Self {
            change,
            enabled,
            skip_first_turn: false,
        }
    }
}

/// The thread accepts a hand-off only after the note is durable and the old
/// session's input queue is reserved for the replacement.
#[derive(Clone, Debug)]
pub struct Decision(Arc<Mutex<Option<oneshot::Sender<bool>>>>);

impl PartialEq for Decision {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Decision {
    pub fn answer(&self, accepted: bool) {
        if let Some(sender) = self.0.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = sender.send(accepted);
        }
    }
}

pub(super) struct Boundary {
    experiment: Experiment,
    crossed: bool,
    original: Option<Result<StopReason, String>>,
}

impl Boundary {
    pub(super) fn new(experiment: Experiment) -> Self {
        Self {
            experiment,
            crossed: false,
            original: None,
        }
    }
}

pub(super) enum TurnBoundary {
    Finish(Result<StopReason, String>),
    Summarizing,
    Replace(Result<StopReason, String>),
}

impl Turn<'_> {
    pub(super) async fn handoff_boundary(
        &self,
        boundary: &mut Option<Boundary>,
        runs: &RunResults,
        outcome: Result<StopReason, String>,
        context: Option<u64>,
    ) -> Result<TurnBoundary, agent_client_protocol::Error> {
        let Some(boundary) = boundary else {
            return Ok(TurnBoundary::Finish(outcome));
        };
        if std::mem::take(&mut boundary.experiment.skip_first_turn) {
            return Ok(TurnBoundary::Finish(outcome));
        }
        if let Some(original) = boundary.original.take() {
            return Ok(self
                .finish_note(&boundary.experiment.change, runs, outcome, original)
                .await);
        }
        if boundary.crossed
            || !matches!(outcome, Ok(StopReason::EndTurn))
            || context.is_none_or(|tokens| tokens < CONTEXT_THRESHOLD)
            || runs.outstanding()
            || !runs.ready.is_empty()
        {
            return Ok(TurnBoundary::Finish(outcome));
        }
        boundary.crossed = true;
        let (decision, reply) = if boundary.experiment.enabled {
            let (decision, reply) = decision_channel();
            (Some(decision), Some(reply))
        } else {
            (None, None)
        };
        self.shared.emit(AgentEvent::HandoffThreshold {
            change: boundary.experiment.change.clone(),
            decision,
        });
        if let Some(reply) = reply
            && reply.await.unwrap_or(false)
        {
            *self
                .shared
                .handoff_text
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some(String::new());
            if let Err(error) = self.request(vec![ContentBlock::from(PROMPT)]) {
                self.shared
                    .handoff_text
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                return Err(error);
            }
            boundary.original = Some(outcome);
            return Ok(TurnBoundary::Summarizing);
        }
        Ok(TurnBoundary::Finish(outcome))
    }

    async fn finish_note(
        &self,
        change: &str,
        runs: &RunResults,
        outcome: Result<StopReason, String>,
        original: Result<StopReason, String>,
    ) -> TurnBoundary {
        let note = self
            .shared
            .handoff_text
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if matches!(outcome, Ok(StopReason::EndTurn))
            && let Some(note) = note.filter(|note| valid(note))
            && !runs.outstanding()
            && runs.ready.is_empty()
        {
            let (decision, reply) = decision_channel();
            self.shared.emit(AgentEvent::HandoffReady {
                change: change.into(),
                note,
                decision,
            });
            if reply.await.unwrap_or(false) {
                return TurnBoundary::Replace(original);
            }
        }
        TurnBoundary::Finish(original)
    }
}

fn decision_channel() -> (Decision, oneshot::Receiver<bool>) {
    let (sender, receiver) = oneshot::channel();
    (Decision(Arc::new(Mutex::new(Some(sender)))), receiver)
}

pub(super) fn capture(shared: &Shared, update: &SessionUpdate) -> bool {
    let mut capture = shared
        .handoff_text
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let Some(note) = capture.as_mut() else {
        return false;
    };
    match update {
        SessionUpdate::AgentMessageChunk(chunk) => {
            if let ContentBlock::Text(text) = &chunk.content
                && note.len() <= MAX_NOTE_BYTES
            {
                let limit = (MAX_NOTE_BYTES + 1 - note.len()).min(text.text.len());
                note.push_str(&text.text[..text.text.floor_char_boundary(limit)]);
                if limit < text.text.len() {
                    // Preserve an overflow marker even when the chunk ends inside a character.
                    while note.len() <= MAX_NOTE_BYTES {
                        note.push(' ');
                    }
                }
            }
            true
        }
        SessionUpdate::AgentThoughtChunk(_) => true,
        _ => false,
    }
}

fn valid(note: &str) -> bool {
    if note.len() > MAX_NOTE_BYTES {
        return false;
    }
    let mut sections = note.trim().lines().peekable();
    for heading in HEADINGS {
        let Some(title) = sections.next().and_then(|line| line.strip_prefix("## ")) else {
            return false;
        };
        let mut body = false;
        while sections.peek().is_some_and(|line| !line.starts_with("## ")) {
            body |= sections.next().is_some_and(|line| !line.trim().is_empty());
        }
        if title.trim_end() != heading || !body {
            return false;
        }
    }
    sections.next().is_none()
}
