//! Rosen threads' progress, from their project's Rosen event stream (F5, F6). Rosen's events
//! take the place of the GitHub PR watcher for threads that work in Rosen workspaces.

use serde::{Deserialize, Serialize};
use velocity_core::thread_view_model::TranscriptBlock;

use crate::{AgentActivity, ReviewStage, StateStore, StoreError, ThreadItem, ThreadState};

/// What a Rosen event means for the thread whose change it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosenProgress {
    /// The agent named its work with `rosen commit -m`: it is ready for review.
    Named,
    /// The change landed on the base: the thread is merged.
    Landed,
    /// Post-land verification reverted the landed change and gave it back to its owner
    /// (Rosen R12): the thread needs fixing.
    Reverted,
    /// PR duty requested changes (F7): the thread fixes them, and is reviewed again when it
    /// names its work again.
    ChangesRequested,
}

/// What a Rosen event leaves Velocity to do once its thread's new state is saved (F5): stop
/// a landed thread's agent, or give a reverted or sent-back thread its workspace and its
/// agent the message. Saved in the same snapshot as the state and the stream's cursor, and
/// removed only when done, so acknowledging an event never loses the work it asks for: after
/// a crash or a failed attempt, the next pass does it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RosenFollowUp {
    /// The change landed: its agent and watcher stop and its AIRC claims are released.
    FinishLanded,
    /// The change was reverted: its workspace is made again and its agent resumed with
    /// `message`.
    Reopen { message: String, mark: usize },
    /// PR duty sent the change back: its agent gets `message`.
    Deliver { message: String, mark: usize },
}

impl RosenFollowUp {
    /// The message for the thread's agent, if this follow-up sends one.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::FinishLanded => None,
            Self::Reopen { message, .. } | Self::Deliver { message, .. } => Some(message),
        }
    }

    /// Whether `thread`'s saved transcript already has this follow-up's message, added after
    /// it was recorded: a pass that sent it was cut off before it could remove the follow-up,
    /// and sending it again would give the agent the same work twice. A message still queued
    /// there is sent by the restart's resume (SPEC.md §3.2).
    pub fn delivered(&self, thread: &ThreadItem) -> bool {
        let (Self::Reopen { message, mark } | Self::Deliver { message, mark }) = self else {
            return false;
        };
        thread.transcript.iter().skip(*mark).any(
            |block| matches!(block, TranscriptBlock::UserMessage { text, .. } if text == message),
        )
    }
}

impl StateStore {
    /// Applies one event of `project`'s Rosen stream to the thread working on `change`, and
    /// saves the stream's cursor `seq` with it, so a restart resumes after it. What the event
    /// leaves to do is saved in the same snapshot as a [`RosenFollowUp`]; `message` is the one
    /// a revert or a request for changes sends the agent. Returns that thread's id; `None`
    /// when no thread works on `change`.
    pub fn save_rosen_progress(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        (progress, message): (RosenProgress, &str),
    ) -> Result<Option<u32>, StoreError> {
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(None);
        };
        item.rosen_event_cursor = item.rosen_event_cursor.max(seq);
        let thread = item
            .threads
            .iter_mut()
            .find(|thread| thread.rosen_change.as_deref() == Some(change));
        let Some(thread) = thread else {
            self.store_snapshot(next)?;
            return Ok(None);
        };
        let identifier = thread.id;
        let landed = apply_progress(thread, progress);
        queue_follow_up(thread, progress, message);
        if landed {
            crate::follow_through::finish_thread_tasks(&mut next, project, identifier);
        }
        self.store_snapshot(next)?;
        Ok(Some(identifier))
    }

    /// Every thread of `project` with follow-ups left, each with its follow-ups and change.
    pub fn rosen_follow_ups(&self, project: u32) -> Vec<(u32, Option<String>, Vec<RosenFollowUp>)> {
        self.project(project).map_or_else(Vec::new, |project| {
            project
                .threads
                .iter()
                .filter(|thread| !thread.rosen_follow_ups.is_empty())
                .map(|thread| {
                    let follow_ups = thread.rosen_follow_ups.clone();
                    (thread.id, thread.rosen_change.clone(), follow_ups)
                })
                .collect()
        })
    }

    /// Whether `follow_up`'s message already reached thread `thread`'s saved transcript.
    pub fn rosen_follow_up_delivered(
        &self,
        project: u32,
        thread: u32,
        follow_up: &RosenFollowUp,
    ) -> bool {
        self.project(project)
            .and_then(|project| project.threads.iter().find(|item| item.id == thread))
            .is_some_and(|thread| follow_up.delivered(thread))
    }

    /// Removes `done`, the oldest of thread `thread`'s follow-ups, once it is finished.
    pub fn finish_rosen_follow_up(
        &mut self,
        project: u32,
        thread: u32,
        done: &RosenFollowUp,
    ) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .and_then(|project| project.threads.iter_mut().find(|item| item.id == thread));
        let Some(item) = item else {
            return Ok(());
        };
        let Some(index) = item
            .rosen_follow_ups
            .iter()
            .position(|pending| pending == done)
        else {
            return Ok(());
        };
        item.rosen_follow_ups.remove(index);
        self.store_snapshot(next)
    }

    /// The newest Rosen event `project`'s threads have taken in; 0 before the first.
    pub fn rosen_event_cursor(&self, project: u32) -> u64 {
        self.project(project)
            .map_or(0, |project| project.rosen_event_cursor)
    }
}

/// Records what `progress` leaves to do. A land ends the thread's work, so it replaces
/// anything still waiting: a revert's message must not resume an agent the land stopped.
fn queue_follow_up(thread: &mut ThreadItem, progress: RosenProgress, message: &str) {
    let mark = thread.transcript.len();
    let follow_up = match progress {
        RosenProgress::Named => return,
        RosenProgress::Landed => {
            thread.rosen_follow_ups.clear();
            RosenFollowUp::FinishLanded
        }
        RosenProgress::Reverted => RosenFollowUp::Reopen {
            message: message.to_owned(),
            mark,
        },
        // A request for changes on a merged thread is stale: the land stopped its agent.
        RosenProgress::ChangesRequested if thread.state == ThreadState::Merged => return,
        RosenProgress::ChangesRequested => RosenFollowUp::Deliver {
            message: message.to_owned(),
            mark,
        },
    };
    thread.rosen_follow_ups.push(follow_up);
}

/// Moves the thread through review as its change does. Returns true when the thread has
/// just become merged, so its tasks are done.
fn apply_progress(thread: &mut ThreadItem, progress: RosenProgress) -> bool {
    match progress {
        // A replayed event never takes a merged thread back to review.
        RosenProgress::Named if thread.state != ThreadState::Merged => {
            thread.state = ThreadState::Review;
            thread.review_stage = Some(ReviewStage::Review);
            false
        }
        RosenProgress::Named => false,
        RosenProgress::Landed => {
            let landed = thread.state != ThreadState::Merged;
            thread.state = ThreadState::Merged;
            thread.review_stage = Some(ReviewStage::Approved);
            thread.activity = AgentActivity::Idle;
            landed
        }
        RosenProgress::Reverted => {
            thread.state = ThreadState::Review;
            thread.review_stage = Some(ReviewStage::Fixing);
            false
        }
        RosenProgress::ChangesRequested if thread.state != ThreadState::Merged => {
            thread.state = ThreadState::Review;
            thread.review_stage = Some(ReviewStage::Fixing);
            false
        }
        RosenProgress::ChangesRequested => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ProjectItem;

    // Risk (audit of 59be908, finding b): a revert's cursor and state were saved before its
    // workspace, runtime and message were redone, so a crash in between acknowledged the
    // event with the work undone, and nothing after a restart did it. The follow-up must be
    // saved with the cursor, survive a reload, stay until finished, and not send its message
    // a second time once the transcript has it.
    #[test]
    fn a_reverts_follow_up_survives_a_restart_until_it_is_done()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-rosen-follow-up-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&directory);
        let mut store = StateStore::load_or_default(&directory)?;
        let mut project = ProjectItem::new(1, "One".into(), r"C:\project one".into());
        let mut thread = ThreadItem::new(4, "Work".into());
        thread.rosen_change = Some("abc".into());
        thread.state = ThreadState::Merged;
        project.threads.push(thread);
        store.state.projects = vec![project];
        let message = "The change was reverted: fix the failing test.";
        store.save_rosen_progress(1, 9, "abc", (RosenProgress::Reverted, message))?;

        // A crash here: the next start finds the cursor and the work still to do.
        let mut reloaded = StateStore::load_or_default(&directory)?;
        assert_eq!(reloaded.rosen_event_cursor(1), 9);
        let pending = reloaded.rosen_follow_ups(1);
        let [(4, Some(change), follow_ups)] = pending.as_slice() else {
            panic!("the follow-up was lost: {pending:?}");
        };
        assert_eq!(change, "abc");
        let [follow_up] = follow_ups.as_slice() else {
            panic!("expected one follow-up: {follow_ups:?}");
        };
        assert_eq!(follow_up.message(), Some(message));
        assert!(!reloaded.rosen_follow_up_delivered(1, 4, follow_up));

        // A pass sent the message and was cut off before removing the follow-up.
        let mut sent = reloaded.state.clone();
        let blocks = vec![TranscriptBlock::UserMessage {
            text: message.into(),
            queued: true,
        }];
        sent.projects[0].threads[0].transcript = Arc::new(blocks);
        reloaded.store_snapshot(sent)?;
        assert!(reloaded.rosen_follow_up_delivered(1, 4, follow_up));

        reloaded.finish_rosen_follow_up(1, 4, follow_up)?;
        let finished = StateStore::load_or_default(&directory)?;
        assert!(finished.rosen_follow_ups(1).is_empty());
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }
}
