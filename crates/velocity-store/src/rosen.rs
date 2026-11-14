//! Rosen threads' progress, from their project's Rosen event stream (F5, F6). Rosen's events
//! take the place of the GitHub PR watcher for threads that work in Rosen workspaces.

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

impl StateStore {
    /// Applies one event of `project`'s Rosen stream to the thread working on `change`, and
    /// saves the stream's cursor `seq` with it, so a restart resumes after it. Returns that
    /// thread's id, whether or not its state changed, so the caller can finish a landed
    /// thread again after a crash; `None` when no thread works on `change`.
    pub fn save_rosen_progress(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        progress: RosenProgress,
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
        if landed {
            crate::follow_through::finish_thread_tasks(&mut next, project, identifier);
        }
        self.store_snapshot(next)?;
        Ok(Some(identifier))
    }

    /// The newest Rosen event `project`'s threads have taken in; 0 before the first.
    pub fn rosen_event_cursor(&self, project: u32) -> u64 {
        self.project(project)
            .map_or(0, |project| project.rosen_event_cursor)
    }
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
