//! Messages the window shows as sent before their agent has them (#318). A running
//! thread is drawn from its live transcript, and each agent update replaces that whole
//! transcript with the agent's own, which lacks a message still waiting in the send
//! queue. These are added back to every update until the agent's transcript shows them.
use crate::thread_view_model::TranscriptBlock;

#[derive(Clone, Default)]
pub(super) struct PendingSends(Vec<PendingSend>);

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
        self.0.push(PendingSend {
            thread,
            text: text.to_owned(),
            queued,
            shown_after,
            delivered: false,
        });
    }

    /// Drops the newest pending message with this text: its send failed.
    pub(super) fn forget(&mut self, thread: (u32, usize), text: &str) {
        if let Some(index) = self
            .0
            .iter()
            .rposition(|pending| pending.thread == thread && pending.text == text)
        {
            self.0.remove(index);
        }
    }

    /// Marks the oldest waiting message with this text as taken by the agent.
    pub(super) fn delivered(&mut self, thread: (u32, usize), text: &str) {
        if let Some(pending) = self
            .0
            .iter_mut()
            .find(|pending| pending.thread == thread && pending.text == text && !pending.delivered)
        {
            pending.delivered = true;
        }
    }

    /// Forgets the messages the agent's transcript now shows, and appends the rest.
    pub(super) fn add_to(&mut self, thread: (u32, usize), blocks: &mut Vec<TranscriptBlock>) {
        self.0.retain(|pending| {
            pending.thread != thread
                || (!pending.delivered
                    && count_messages(blocks, &pending.text) <= pending.shown_after)
        });
        for pending in self.0.iter().filter(|pending| pending.thread == thread) {
            blocks.push(TranscriptBlock::UserMessage {
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
