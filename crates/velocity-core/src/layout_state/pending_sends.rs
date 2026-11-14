//! Messages the window shows as sent before their agent has them (#318). A running
//! thread is drawn from its live transcript, and each agent update replaces that whole
//! transcript with the agent's own, which lacks a message still waiting in the send
//! queue. These are added back to every update until the agent's transcript shows them.
//!
//! Sends to one thread reach their agent in order, so each answer (delivered or failed)
//! belongs to the oldest unanswered send with its text.
use crate::thread_view_model::{SharedBlocks, TranscriptBlock};

#[derive(Clone, Default)]
pub(super) struct PendingSends {
    waiting: Vec<PendingSend>,
    /// Sends the agent's transcript showed before their answer came back. Their answer
    /// still arrives, and must not be taken for a later send with the same text.
    shown_before_answer: Vec<((u32, usize), String)>,
}

#[derive(Clone)]
struct PendingSend {
    thread: (u32, usize),
    text: String,
    queued: bool,
    /// How many messages with this text the agent's transcript must hold before it holds
    /// this one: those it had when this was sent, plus earlier pending ones like it.
    shown_after: usize,
    /// The agent took it. Its transcript holds it from the next update on, whatever
    /// text it shows, so that update ends the wait even if the count never matches.
    delivered: bool,
}

impl PendingSends {
    /// Records a message just added to `blocks`, the thread's live transcript.
    pub(super) fn record(
        &mut self,
        thread: (u32, usize),
        blocks: &[TranscriptBlock],
        text: &str,
        queued: bool,
    ) {
        // `blocks` already holds this message and the earlier pending ones like it.
        let shown_after = count_messages(blocks, text).saturating_sub(1);
        self.waiting.push(PendingSend {
            thread,
            text: text.to_owned(),
            queued,
            shown_after,
            delivered: false,
        });
    }

    /// The oldest unanswered send with this text failed: it leaves the transcript.
    pub(super) fn forget(&mut self, thread: (u32, usize), text: &str) {
        if self.answered_early(thread, text) {
            return;
        }
        if let Some(index) = self.unanswered(thread, text) {
            self.waiting.remove(index);
        }
    }

    /// The oldest unanswered send with this text reached the agent.
    pub(super) fn delivered(&mut self, thread: (u32, usize), text: &str) {
        if self.answered_early(thread, text) {
            return;
        }
        if let Some(index) = self.unanswered(thread, text) {
            self.waiting[index].delivered = true;
        }
    }

    /// Whether this answer belongs to a send the transcript already showed; if so, that
    /// send is now fully answered.
    fn answered_early(&mut self, thread: (u32, usize), text: &str) -> bool {
        let Some(index) = self
            .shown_before_answer
            .iter()
            .position(|(shown, shown_text)| *shown == thread && shown_text == text)
        else {
            return false;
        };
        self.shown_before_answer.remove(index);
        true
    }

    fn unanswered(&self, thread: (u32, usize), text: &str) -> Option<usize> {
        self.waiting.iter().position(|pending| {
            pending.thread == thread && pending.text == text && !pending.delivered
        })
    }

    /// Forgets the messages the agent's transcript now shows, and appends the rest.
    pub(super) fn add_to(&mut self, thread: (u32, usize), blocks: &mut SharedBlocks) {
        let mut shown = Vec::new();
        self.waiting.retain(|pending| {
            if pending.thread != thread {
                return true;
            }
            if pending.delivered {
                return false;
            }
            let waiting = count_messages(blocks, &pending.text) <= pending.shown_after;
            if !waiting {
                shown.push((thread, pending.text.clone()));
            }
            waiting
        });
        self.shown_before_answer.extend(shown);
        // The agent's blocks are shared with its runtime; copy them only to add to them.
        for pending in self
            .waiting
            .iter()
            .filter(|pending| pending.thread == thread)
        {
            std::sync::Arc::make_mut(blocks).push(TranscriptBlock::UserMessage {
                text: pending.text.clone(),
                queued: pending.queued,
            });
        }
    }
}

fn count_messages(blocks: &[TranscriptBlock], text: &str) -> usize {
    blocks
        .iter()
        .filter(|block| matches!(block, TranscriptBlock::UserMessage { text: shown, .. } if shown == text))
        .count()
}
