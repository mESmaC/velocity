//! Rosen threads' progress, from their project's Rosen event stream (F5, F6). Rosen's events
//! take the place of the GitHub PR watcher for threads that work in Rosen workspaces.

use serde::{Deserialize, Serialize};
use velocity_core::thread_view_model::{RosenChangeStage, RosenChangeView, TranscriptBlock};

use crate::rosen_work::WorkEvidence;
use crate::{AgentActivity, ReviewStage, StateStore, StoreError, ThreadItem, ThreadState};

/// What a Rosen event means for the thread whose change it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosenProgress {
    /// The agent named its work with `rosen commit -m`: it is ready for review.
    Named,
    /// The change landed on the base: the thread is merged.
    Landed,
    /// Rosen closed the change and released its path claims.
    Abandoned,
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
    FinishAbandoned,
    /// The change was reverted: its workspace is made again and its agent resumed with
    /// `message`.
    Reopen {
        message: String,
        mark: usize,
    },
    /// PR duty sent the change back: its agent gets `message`.
    Deliver {
        message: String,
        mark: usize,
    },
    /// Apply the current head at a turn boundary before delivering this conflict notice.
    Conflict {
        #[serde(default)]
        message: String,
        #[serde(default)]
        mark: usize,
    },
    /// Apply the rebased head before asking for regeneration, even without conflicts.
    Regenerate {
        #[serde(default)]
        message: String,
        #[serde(default)]
        mark: usize,
    },
}

impl RosenFollowUp {
    /// The message for the thread's agent, if this follow-up sends one.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::FinishLanded | Self::FinishAbandoned => None,
            Self::Reopen { message, .. }
            | Self::Deliver { message, .. }
            | Self::Conflict { message, .. }
            | Self::Regenerate { message, .. } => Some(message),
        }
    }

    /// Whether `thread`'s saved transcript already has this follow-up's message, added after
    /// it was recorded: a pass that sent it was cut off before it could remove the follow-up,
    /// and sending it again would give the agent the same work twice. A message still queued
    /// there is sent by the restart's resume (SPEC.md §3.2).
    pub fn delivered(&self, thread: &ThreadItem) -> bool {
        let (Self::Reopen { message, mark }
        | Self::Deliver { message, mark }
        | Self::Conflict { message, mark }
        | Self::Regenerate { message, mark }) = self
        else {
            return false;
        };
        thread.transcript.iter().skip(*mark).any(
            |block| matches!(block, TranscriptBlock::UserMessage { text, .. } if text == message),
        )
    }
}

impl StateStore {
    /// Put the Resolve action through the same durable turn-boundary delivery as a conflict event.
    pub fn queue_rosen_resolution(
        &mut self,
        project: u32,
        identifier: u32,
        change: &str,
        message: String,
    ) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        let Some(thread) = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .and_then(|item| {
                item.threads.iter_mut().find(|thread| {
                    thread.id == identifier
                        && thread.rosen_change.as_deref() == Some(change)
                        && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
                })
            })
        else {
            return Ok(());
        };
        if thread
            .rosen_follow_ups
            .iter()
            .any(|pending| matches!(pending, RosenFollowUp::Conflict { .. }))
        {
            return Ok(());
        }
        thread.rosen_follow_ups.push(RosenFollowUp::Conflict {
            message,
            mark: thread.transcript.len(),
        });
        self.store_snapshot(next)
    }

    /// Updates the cached view only for the same registered change. Read failures retain the
    /// last view and a later follower pass retries the read.
    pub fn save_rosen_change_views(
        &mut self,
        project: u32,
        mut views: std::collections::BTreeMap<String, (u64, RosenChangeView)>,
    ) -> Result<bool, StoreError> {
        let Some(current) = self.project(project) else {
            return Ok(false);
        };
        views.retain(|change, (generation, view)| {
            current.threads.iter().any(|thread| {
                thread.rosen_change.as_ref() == Some(change)
                    && thread.rosen_progress_generation == *generation
                    && thread.rosen_change_view.as_ref() != Some(view)
            })
        });
        if views.is_empty() {
            return Ok(false);
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            for thread in &mut item.threads {
                if let Some((_, view)) = thread.rosen_change.as_ref().and_then(|id| views.get(id)) {
                    thread.rosen_change_view = Some(view.clone());
                }
            }
        }
        self.store_snapshot(next)?;
        Ok(true)
    }

    /// Save a conflict notice with its event cursor, without sending it to a busy agent
    /// whose workspace still holds the old basis. Superseded pending notices coalesce.
    pub fn save_rosen_conflict(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        message: &str,
    ) -> Result<(), StoreError> {
        self.save_rosen_rebase_notice(project, seq, change, message, false)
    }

    /// Save generated-file work with the rebase cursor, including conflict-free rebases.
    pub fn save_rosen_regeneration(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        message: &str,
    ) -> Result<(), StoreError> {
        self.save_rosen_rebase_notice(project, seq, change, message, true)
    }

    fn save_rosen_rebase_notice(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        message: &str,
        regeneration: bool,
    ) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(());
        };
        if seq <= item.rosen_event_cursor {
            return Ok(());
        }
        item.rosen_event_cursor = seq;
        if let Some(thread) = item.threads.iter_mut().find(|thread| {
            thread.rosen_change.as_deref() == Some(change) && thread.state != ThreadState::Merged
        }) {
            if seq <= item.rosen_replay_through && notice_handled(thread, message) {
                return self.store_snapshot(next);
            }
            // Regeneration is cumulative: later rebases may omit still-pending files.
            // A text-conflict notice must not discard that work either.
            let notice = if regeneration {
                RosenFollowUp::Regenerate {
                    message: message.to_owned(),
                    mark: thread.transcript.len(),
                }
            } else {
                thread
                    .rosen_follow_ups
                    .retain(|pending| !matches!(pending, RosenFollowUp::Conflict { .. }));
                RosenFollowUp::Conflict {
                    message: message.to_owned(),
                    mark: thread.transcript.len(),
                }
            };
            thread.rosen_follow_ups.push(notice);
        }
        self.store_snapshot(next)
    }

    /// Validate the saved checkpoints before subscribing. An unbound position
    /// from an older Velocity build is replayed once.
    pub fn resume_rosen_events(
        &mut self,
        project: u32,
        identity: &str,
        latest: u64,
    ) -> Result<(u64, u64, bool), StoreError> {
        let Some(current) = self.project(project) else {
            return Ok((0, 0, false));
        };
        let changed = current.rosen_event_server_identity.as_deref() != Some(identity);
        let past_end = current.rosen_event_cursor > latest || current.rosen_replay_through > latest;
        if !changed && !past_end {
            return Ok((
                current.rosen_event_cursor,
                current.rosen_replay_through,
                false,
            ));
        }
        let replacing = changed && current.rosen_event_server_identity.is_some();
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok((0, 0, false));
        };
        let previous = item.rosen_event_cursor;
        item.rosen_legacy_replay_cursor = if item.rosen_event_server_identity.is_none() && !past_end
        {
            previous
        } else {
            0
        };
        item.rosen_event_cursor = 0;
        item.rosen_event_server_identity = Some(identity.to_owned());
        item.rosen_replay_through = latest;
        for thread in &mut item.threads {
            if replacing || thread.rosen_import_event > latest {
                thread.rosen_import_event = 0;
            }
            thread.rosen_issue.rewind_events(replacing, latest);
        }
        for handoff in item.handoff_items.values_mut() {
            handoff.rewind_events(replacing, latest);
        }
        self.store_snapshot(next)?;
        if previous != 0 || replacing || past_end {
            eprintln!(
                "Rosen events for project {project}: restarting at 0 (saved {previous}, latest {latest}, database changed: {changed})"
            );
        }
        Ok((0, latest, true))
    }

    /// Retire a closed thread's link only if it still refers to the failed lookup.
    pub fn forget_missing_rosen_changes(
        &mut self,
        project: u32,
        missing: &std::collections::BTreeMap<String, u64>,
    ) -> Result<bool, StoreError> {
        if missing.is_empty() {
            return Ok(false);
        }
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(false);
        };
        let mut changed = false;
        for thread in &mut item.threads {
            if matches!(
                thread.state,
                ThreadState::Done | ThreadState::Closed | ThreadState::Merged
            ) && thread
                .rosen_change
                .as_ref()
                .and_then(|change| missing.get(change))
                == Some(&thread.rosen_progress_generation)
            {
                thread.rosen_change = None;
                changed = true;
            }
        }
        if changed {
            self.store_snapshot(next)?;
        }
        Ok(changed)
    }

    /// Rebases without regeneration need no saved action: the periodic pass reads the basis.
    /// Their cursor still advances so reconnecting never replays older conflict events.
    pub fn save_rosen_cursor(&mut self, project: u32, seq: u64) -> Result<(), StoreError> {
        if seq <= self.rosen_event_cursor(project) {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            item.rosen_event_cursor = seq;
            self.store_snapshot(next)?;
        }
        Ok(())
    }

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
        // Cursor-zero callers have no event ordering. A land reply must instead carry
        // the generation it acknowledged through checkpoint_rosen_land.
        if seq == 0 && progress == RosenProgress::Landed {
            return Ok(None);
        }
        self.record_rosen_progress(
            project,
            seq,
            change,
            (progress, message),
            None,
            WorkEvidence::default(),
        )
    }

    pub fn save_rosen_work_progress(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        progress: (RosenProgress, &str),
        evidence: WorkEvidence<'_>,
    ) -> Result<Option<u32>, StoreError> {
        self.record_rosen_progress(project, seq, change, progress, None, evidence)
    }

    pub fn save_rosen_named(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        commit: Option<&str>,
    ) -> Result<Option<u32>, StoreError> {
        self.record_rosen_progress(
            project,
            seq,
            change,
            (RosenProgress::Named, ""),
            commit,
            WorkEvidence::default(),
        )
    }

    /// Checkpoint the named work the checker landed only while its saved lifecycle is
    /// unchanged. A newer event or fixing state owns its follow-ups and tasks instead.
    pub fn checkpoint_rosen_land(
        &mut self,
        project: u32,
        change: &str,
        generation: u64,
    ) -> Result<Option<u32>, StoreError> {
        self.checkpoint_rosen_progress(project, change, generation, (RosenProgress::Landed, ""))
    }

    /// Record a refusal and queue its author message only for the generation that was refused.
    pub fn checkpoint_rosen_land_refusal(
        &mut self,
        project: u32,
        change: &str,
        generation: u64,
        message: &str,
    ) -> Result<Option<u32>, StoreError> {
        self.checkpoint_rosen_progress(
            project,
            change,
            generation,
            (RosenProgress::ChangesRequested, message),
        )
    }

    fn checkpoint_rosen_progress(
        &mut self,
        project: u32,
        change: &str,
        generation: u64,
        progress: (RosenProgress, &str),
    ) -> Result<Option<u32>, StoreError> {
        let current = self.project(project).and_then(|project| {
            project.threads.iter().find(|thread| {
                thread.rosen_change.as_deref() == Some(change)
                    && thread.rosen_progress_generation == generation
                    && thread.state == ThreadState::Review
                    && matches!(
                        thread.review_stage,
                        Some(ReviewStage::Review | ReviewStage::Approved)
                    )
            })
        });
        if current.is_none() {
            return Ok(None);
        }
        self.record_rosen_progress(project, 0, change, progress, None, WorkEvidence::default())
    }

    fn record_rosen_progress(
        &mut self,
        project: u32,
        seq: u64,
        change: &str,
        (progress, message): (RosenProgress, &str),
        named_commit: Option<&str>,
        evidence: WorkEvidence<'_>,
    ) -> Result<Option<u32>, StoreError> {
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(None);
        };
        if seq != 0 && seq <= item.rosen_event_cursor {
            return Ok(None);
        }
        item.rosen_event_cursor = item.rosen_event_cursor.max(seq);
        let thread = item
            .threads
            .iter()
            .position(|thread| thread.rosen_change.as_deref() == Some(change));
        let Some(index) = thread else {
            crate::rosen_work::apply(item, change, progress, evidence);
            self.store_snapshot(next)?;
            return Ok(None);
        };
        let identifier = item.threads[index].id;
        crate::rosen_work::restore_thread(item, identifier);
        if progress != RosenProgress::Named {
            crate::rosen_work::apply(item, change, progress, evidence);
        }
        let thread = &mut item.threads[index];
        let replaying = seq != 0 && seq <= item.rosen_replay_through;
        // Before named commits were tracked, a fixing decision at an in-range
        // checkpoint is evidence that its earlier name already moved to Review.
        if replaying
            && progress == RosenProgress::Named
            && thread.review_stage == Some(ReviewStage::Fixing)
            && thread.rosen_last_named_commit.is_none()
            && seq <= item.rosen_legacy_replay_cursor
        {
            thread.rosen_last_named_commit = named_commit.map(str::to_owned);
            self.store_snapshot(next)?;
            return Ok(None);
        }
        if already_applied(thread, seq, replaying, (progress, message), named_commit) {
            self.store_snapshot(next)?;
            return Ok(None);
        }
        if progress == RosenProgress::Named {
            thread.rosen_last_named_commit = named_commit.map(str::to_owned);
            crate::rosen_work::apply(item, change, progress, evidence);
        }
        let thread = &mut item.threads[index];
        if progress == RosenProgress::ChangesRequested
            && thread.state != ThreadState::Merged
            && thread.review_stage != Some(ReviewStage::Fixing)
            && let Some(item) = item.handoff_items.get_mut(change)
        {
            item.requested_rounds = item.requested_rounds.saturating_add(1);
        }
        thread.rosen_progress_generation = thread.rosen_progress_generation.saturating_add(1);
        let reverted = progress == RosenProgress::Reverted && thread.state == ThreadState::Merged;
        let abandoned = progress == RosenProgress::Abandoned;
        let landed = apply_progress(thread, progress, message);
        queue_follow_up(thread, progress, message);
        crate::goal_rosen::apply(item, identifier, progress);
        crate::swarms::record_rosen_activity(item, identifier, progress);
        if abandoned {
            for task in &mut item.tasks {
                if task.assigned_thread_id == Some(identifier)
                    && task.state == crate::TaskState::Now
                {
                    task.assigned_thread_id = None;
                    task.state = crate::TaskState::Todo;
                }
            }
        }
        if landed {
            crate::follow_through::finish_thread_tasks(&mut next, project, identifier);
        }
        if reverted {
            crate::follow_through::reopen_thread_tasks(&mut next, project, identifier);
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

fn already_applied(
    thread: &ThreadItem,
    sequence: u64,
    replaying: bool,
    (progress, message): (RosenProgress, &str),
    named_commit: Option<&str>,
) -> bool {
    match progress {
        RosenProgress::Named => {
            thread.state == ThreadState::Merged
                || (sequence != 0 && sequence <= thread.rosen_import_event)
                || (replaying
                    && named_commit.is_some()
                    && thread.rosen_last_named_commit.as_deref() == named_commit)
        }
        RosenProgress::Landed => thread.state == ThreadState::Merged,
        RosenProgress::Abandoned => {
            matches!(thread.state, ThreadState::Done | ThreadState::Merged)
        }
        RosenProgress::Reverted if replaying => {
            thread.state != ThreadState::Merged && notice_handled(thread, message)
        }
        _ => false,
    }
}

fn notice_handled(thread: &ThreadItem, message: &str) -> bool {
    thread
        .rosen_follow_ups
        .iter()
        .any(|notice| notice.message() == Some(message))
        || thread.transcript.iter().any(
            |block| matches!(block, TranscriptBlock::UserMessage { text, .. } if text == message),
        )
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
        RosenProgress::Abandoned => {
            thread.rosen_follow_ups.clear();
            RosenFollowUp::FinishAbandoned
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
fn apply_progress(thread: &mut ThreadItem, progress: RosenProgress, message: &str) -> bool {
    thread.rosen_land_refusal = (progress == RosenProgress::ChangesRequested)
        .then(|| message.lines().next().unwrap_or(message).to_owned());
    if let Some(view) = &mut thread.rosen_change_view {
        view.stage = match progress {
            RosenProgress::Named if thread.state != ThreadState::Merged => RosenChangeStage::Named,
            RosenProgress::Landed if view.stage != RosenChangeStage::Green => {
                RosenChangeStage::Landed
            }
            RosenProgress::Abandoned => RosenChangeStage::Abandoned,
            RosenProgress::Reverted => RosenChangeStage::Reopened,
            RosenProgress::ChangesRequested if thread.state != ThreadState::Merged => {
                RosenChangeStage::Editing
            }
            _ => view.stage,
        };
        if view.stage.finished() || view.stage == RosenChangeStage::Reopened {
            view.conflicts.clear();
        }
    }
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
        RosenProgress::Abandoned => {
            thread.state = ThreadState::Done;
            thread.activity = AgentActivity::Idle;
            thread.review_stage = None;
            thread.spec_section = None;
            thread.task_ids.clear();
            false
        }
        RosenProgress::Reverted => {
            thread.state = ThreadState::Review;
            thread.review_stage = Some(ReviewStage::Fixing);
            false
        }
        RosenProgress::ChangesRequested if thread.state != ThreadState::Merged => {
            thread.state = ThreadState::Working;
            thread.review_stage = None;
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

    // Risk (#380 review): an in-flight land reply arrives after a revert, or even after
    // recovery and a new named head. Neither reply may complete reopened work or erase
    // the saved recovery. A current land must still checkpoint normally after a restart.
    #[test]
    fn delayed_land_checkpoint_preserves_recovery_after_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-land-checkpoint-{unique}"));
        let mut store = checkpoint_store(&directory)?;
        let captured = store.state.projects[0].threads[0].rosen_progress_generation;
        store.checkpoint_rosen_land(1, "abc", captured)?;
        store.save_rosen_progress(1, 10, "abc", (RosenProgress::Landed, ""))?;
        store.save_rosen_progress(
            1,
            11,
            "abc",
            (RosenProgress::Reverted, "cargo build failed"),
        )?;
        let mut store = StateStore::load_or_default(&directory)?;
        let recovery = store.rosen_follow_ups(1);
        store.checkpoint_rosen_land(1, "abc", captured)?;
        assert_eq!(
            store.state.projects[0].threads[0].review_stage,
            Some(ReviewStage::Fixing)
        );
        assert_eq!(
            store.state.projects[0].tasks[0].state,
            crate::TaskState::Now
        );
        assert_eq!(store.rosen_follow_ups(1), recovery);
        for (thread, _, steps) in recovery {
            for step in steps {
                store.finish_rosen_follow_up(1, thread, &step)?;
            }
        }
        store.save_rosen_progress(1, 12, "abc", (RosenProgress::Named, ""))?;
        let mut store = StateStore::load_or_default(&directory)?;
        store.checkpoint_rosen_land(1, "abc", captured)?;
        assert_eq!(
            store.state.projects[0].threads[0].review_stage,
            Some(ReviewStage::Review)
        );
        let current = store.state.projects[0].threads[0].rosen_progress_generation;
        store.checkpoint_rosen_land(1, "abc", current)?;
        assert_eq!(
            store.state.projects[0].threads[0].state,
            ThreadState::Merged
        );
        assert_eq!(
            store.state.projects[0].tasks[0].state,
            crate::TaskState::Done
        );
        assert_eq!(store.rosen_event_cursor(1), 12);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn checkpoint_store(directory: &std::path::Path) -> Result<StateStore, StoreError> {
        let mut store = StateStore::load_or_default(directory)?;
        let mut project = ProjectItem::new(1, "One".into(), r"C:\project one".into());
        project.spec_locked = true;
        let mut thread = ThreadItem::new(4, "Work".into());
        thread.rosen_change = Some("abc".into());
        thread.state = ThreadState::Review;
        thread.review_stage = Some(ReviewStage::Review);
        project.threads.push(thread);
        let mut task = crate::TaskItem::new(
            7,
            "Do".into(),
            crate::TaskState::Now,
            crate::TaskAttribution::User,
        );
        task.assigned_thread_id = Some(4);
        project.tasks.push(task);
        store.state.projects = vec![project];
        store.store_snapshot(store.state.clone())?;
        Ok(store)
    }

    // Risk (#482): old positions survive a rebuilt database, suppressing a
    // new name, or a rewind schedules cleanup again for a completed land.
    #[test]
    fn rebuilt_rosen_database_rebinds_saved_positions_after_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-rosen-rewind-{unique}"));
        let mut store = checkpoint_store(&directory)?;
        store.update_thread(1, 4, |thread| thread.state = ThreadState::Working)?;
        store.save_rosen_cursor(1, 475)?;
        let failure = velocity_core::rosen_mirror::MirrorState::new(true, "rejected", None, None);
        store.save_rosen_mirror(1, 142, Some(failure))?;

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.resume_rosen_events(1, "replacement", 96)?,
            (0, 96, true)
        );
        store.resume_rosen_mirror(1, "replacement", 0, None)?;
        store.save_rosen_mirror(1, 0, None)?;
        assert!(store.state.projects[0].rosen_mirror.is_none());
        assert_eq!(store.save_rosen_named(1, 12, "abc", Some("head"))?, Some(4));
        store.save_rosen_progress(1, 13, "abc", (RosenProgress::Landed, ""))?;
        store.finish_rosen_follow_up(1, 4, &RosenFollowUp::FinishLanded)?;

        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store.resume_rosen_events(1, "replacement", 96)?,
            (13, 96, false)
        );
        assert_eq!(
            store.resume_rosen_events(1, "replacement", 23)?,
            (0, 23, true)
        );
        assert_eq!(
            store.save_rosen_progress(1, 13, "abc", (RosenProgress::Landed, ""))?,
            None
        );
        assert!(store.rosen_follow_ups(1).is_empty());
        assert_eq!(store.resume_rosen_events(1, "newer", 500)?, (0, 500, true));
        assert_eq!(
            store.state.projects[0].threads[0].state,
            ThreadState::Merged
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

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
    // Risk (#354): a revert left the landed thread's tasks done and its SPEC section built,
    // so the work pool hid work that was no longer on the base.
    #[test]
    fn a_revert_reopens_the_lands_tasks_and_section() -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-rosen-revert-{}", std::process::id()));
        let _ignored = std::fs::remove_dir_all(&directory);
        let mut store = StateStore::load_or_default(&directory)?;
        let mut project = ProjectItem::new(1, "One".into(), r"C:\project one".into());
        let mut task = crate::TaskItem::new(
            7,
            "Do".into(),
            crate::TaskState::Now,
            crate::TaskAttribution::User,
        );
        task.assigned_thread_id = Some(4);
        project.tasks.push(task);
        let mut thread = ThreadItem::new(4, "Work".into());
        thread.rosen_change = Some("abc".into());
        thread.spec_section = Some("Features / Durable work".into());
        project.threads.push(thread);
        store.state.projects = vec![project];
        store.save_rosen_progress(1, 1, "abc", (RosenProgress::Landed, ""))?;
        store.save_rosen_progress(1, 2, "abc", (RosenProgress::Reverted, "fix it"))?;

        let project = &store.state.projects[0];
        assert_eq!(project.tasks[0].state, crate::TaskState::Now);
        assert_eq!(project.tasks[0].assigned_thread_id, Some(4));
        assert!(project.built_spec_sections.is_empty());
        std::fs::remove_dir_all(&directory)?;
        Ok(())
    }

    // Risk (#487): restarting between a refused land and delivery loses the author's
    // repair prompt, or a delayed refusal sends the newly named head back to Working.
    #[test]
    fn land_refusal_survives_reload_until_the_change_is_named_again()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-land-refusal-{unique}"));
        let mut store = checkpoint_store(&directory)?;
        let refused_generation = store.state.projects[0].threads[0].rosen_progress_generation;
        let reason = "Land refused · regeneration required for Cargo.lock";
        let message = format!("{reason}\n\nRun `rosen rebase` and regenerate Cargo.lock.");
        assert_eq!(
            store.checkpoint_rosen_land_refusal(1, "abc", refused_generation, &message)?,
            Some(4),
        );

        let mut store = StateStore::load_or_default(&directory)?;
        let thread = &store.state.projects[0].threads[0];
        assert_eq!(thread.state, ThreadState::Working);
        assert_eq!(thread.review_stage, None);
        assert_eq!(crate::project_layout::thread_metadata(thread), reason);
        assert!(matches!(
            thread.rosen_follow_ups.as_slice(),
            [RosenFollowUp::Deliver { message: pending, .. }] if pending == &message
        ));

        store.save_rosen_named(1, 10, "abc", Some("repaired-head"))?;
        assert_eq!(
            store.checkpoint_rosen_land_refusal(1, "abc", refused_generation, &message)?,
            None,
        );
        let thread = &store.state.projects[0].threads[0];
        assert_eq!(thread.state, ThreadState::Review);
        assert!(thread.rosen_land_refusal.is_none());
        assert_eq!(
            store.checkpoint_rosen_land(1, "abc", thread.rosen_progress_generation)?,
            Some(4),
        );
        assert_eq!(
            store.state.projects[0].threads[0].state,
            ThreadState::Merged
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
