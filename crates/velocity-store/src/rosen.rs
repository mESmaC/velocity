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

/// Default pending text shown on the Stored conflicts card until delivery or a specific wait (#522).
pub const DEFAULT_RESOLUTION_STATUS: &str = "Resolution queued · sent at the next step boundary";

/// Why a pending conflict resolution has not yet reached the agent (#522).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosenResolutionStatus {
    pub status: String,
    #[serde(default)]
    pub offer_abandon: bool,
}

impl RosenResolutionStatus {
    pub fn new(status: String, offer_abandon: bool) -> Self {
        Self {
            status,
            offer_abandon,
        }
    }
}

/// Durable state for a thread waiting on Rosen path claims to become free (#531).
/// Saved so a release that arrives before the agent finishes its turn, or while Velocity
/// is restarted, still resumes the thread once all waited paths are free.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosenPathWait {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holders: Vec<String>,
    #[serde(default)]
    pub ready_to_resume: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_messages: Vec<String>,
}

impl RosenPathWait {
    pub fn new(paths: Vec<String>, holders: Vec<String>) -> Self {
        let mut wait = Self::default();
        wait.merge(paths, holders);
        wait
    }

    pub fn merge(&mut self, paths: Vec<String>, holders: Vec<String>) {
        for path in paths {
            if !path.trim().is_empty() && !self.paths.contains(&path) {
                self.paths.push(path);
            }
        }
        for holder in holders {
            if !holder.trim().is_empty() && !self.holders.contains(&holder) {
                self.holders.push(holder);
            }
        }
        self.ready_to_resume = false;
    }

    pub fn record_wake(&mut self, path: Option<&str>, released_by: Option<&str>) {
        let message = match (path, released_by) {
            (Some(path), Some(by)) => format!("{path} released by {by}"),
            (Some(path), None) => format!("{path} released"),
            (None, Some(by)) => format!("released by {by}"),
            (None, None) => "waited paths released".into(),
        };
        if !self.wake_messages.contains(&message) {
            self.wake_messages.push(message);
        }
    }

    pub fn description(&self) -> String {
        let paths = if self.paths.is_empty() {
            String::from("paths")
        } else {
            self.paths.join(", ")
        };
        if self.holders.is_empty() {
            format!("waiting on {paths}")
        } else {
            format!("waiting for {} · {paths}", self.holders.join(", "))
        }
    }
}

/// One overlapping open Rosen change that a thread waits on to land or be abandoned (#565).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlapBlocker {
    pub change: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
}

impl OverlapBlocker {
    pub fn label(&self) -> String {
        let short = self.change.get(..8).unwrap_or(&self.change);
        if self.owner.trim().is_empty() {
            short.to_owned()
        } else {
            format!("{} ({short})", self.owner)
        }
    }
}

/// Durable state for a thread waiting on overlapping Rosen changes to land or be abandoned
/// after a preflight overlap failure (#565).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosenOverlapWait {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<OverlapBlocker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wake_messages: Vec<String>,
    /// `PreflightRecord::recorded_at` that created this wait, preventing re-adoption of the
    /// same preflight record after the thread has already woken.
    #[serde(default)]
    pub preflight_recorded_at: u64,
}

impl RosenOverlapWait {
    pub fn from_overlaps(
        overlaps: &[velocity_core::preflight::PreflightOverlap],
        recorded_at: u64,
    ) -> Self {
        let mut blockers = Vec::new();
        for overlap in overlaps {
            if overlap.change.trim().is_empty() {
                continue;
            }
            if let Some(existing) = blockers
                .iter_mut()
                .find(|item: &&mut OverlapBlocker| item.change == overlap.change)
            {
                for path in &overlap.paths {
                    if !path.trim().is_empty() && !existing.paths.contains(path) {
                        existing.paths.push(path.clone());
                    }
                }
            } else {
                blockers.push(OverlapBlocker {
                    change: overlap.change.clone(),
                    owner: overlap.owner.clone(),
                    paths: overlap.paths.clone(),
                });
            }
        }
        Self {
            blockers,
            wake_messages: Vec::new(),
            preflight_recorded_at: recorded_at,
        }
    }

    pub fn blocker_change_ids(&self) -> Vec<String> {
        self.blockers
            .iter()
            .map(|blocker| blocker.change.clone())
            .collect()
    }

    pub fn paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        for blocker in &self.blockers {
            for path in &blocker.paths {
                if !path.trim().is_empty() && !paths.contains(path) {
                    paths.push(path.clone());
                }
            }
        }
        paths
    }

    /// Removes `change` from active blockers and records why it finished.
    pub fn record_resolution(&mut self, change: &str, outcome: &str) -> bool {
        let Some(index) = self
            .blockers
            .iter()
            .position(|blocker| blocker.change == change)
        else {
            return false;
        };
        let blocker = self.blockers.remove(index);
        let message = format!("{} {outcome}", blocker.label());
        if !self.wake_messages.contains(&message) {
            self.wake_messages.push(message);
        }
        true
    }

    /// Drops blockers no longer open on Rosen and records a wake message for each.
    pub fn retain_open(&mut self, open_changes: &std::collections::BTreeSet<String>) -> bool {
        let closed: Vec<String> = self
            .blockers
            .iter()
            .filter(|blocker| !open_changes.contains(&blocker.change))
            .map(|blocker| blocker.change.clone())
            .collect();
        let mut changed = false;
        for change in closed {
            changed |= self.record_resolution(&change, "finished");
        }
        changed
    }

    pub fn record_mutual_break(&mut self) {
        let labels: Vec<String> = self.blockers.iter().map(OverlapBlocker::label).collect();
        let message = if labels.is_empty() {
            String::from("mutual overlap wait detected; proceeding first")
        } else {
            format!(
                "mutual overlap wait with {}; proceeding first",
                labels.join(", ")
            )
        };
        self.blockers.clear();
        if !self.wake_messages.contains(&message) {
            self.wake_messages.push(message);
        }
    }

    pub fn description(&self) -> String {
        let changes: Vec<String> = self.blockers.iter().map(OverlapBlocker::label).collect();
        let paths = self.paths();
        let who = if changes.is_empty() {
            String::from("overlapping change")
        } else {
            changes.join(", ")
        };
        if paths.is_empty() {
            format!("waiting for {who}")
        } else {
            format!("waiting for {who} · {}", paths.join(", "))
        }
    }
}

/// Outcome of queueing a manual conflict resolution for a Rosen thread (#522).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueResolution {
    Queued,
    AlreadyPending,
    Unavailable,
}

impl StateStore {
    /// Put the Resolve action through the same durable turn-boundary delivery as a conflict event.
    /// Returns whether a new resolution was queued or an undelivered one was already pending (#522).
    pub fn queue_rosen_resolution(
        &mut self,
        project: u32,
        identifier: u32,
        change: &str,
        message: String,
        live_transcript_len: usize,
    ) -> Result<QueueResolution, StoreError> {
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
            return Ok(QueueResolution::Unavailable);
        };
        let mark = thread.transcript.len().max(live_transcript_len);
        // Drop any Conflict follow-up that already counts as delivered so an explicit click
        // never finishes silently without sending a prompt (#522).
        drop_delivered_conflicts(thread);
        if let Some(existing) = thread
            .rosen_follow_ups
            .iter_mut()
            .find(|pending| matches!(pending, RosenFollowUp::Conflict { .. }))
        {
            *existing = RosenFollowUp::Conflict { message, mark };
            self.store_snapshot(next)?;
            return Ok(QueueResolution::AlreadyPending);
        }
        thread
            .rosen_follow_ups
            .push(RosenFollowUp::Conflict { message, mark });
        thread.rosen_resolution_status = Some(RosenResolutionStatus::new(
            DEFAULT_RESOLUTION_STATUS.to_owned(),
            false,
        ));
        self.store_snapshot(next)?;
        Ok(QueueResolution::Queued)
    }

    /// Updates the Stored conflicts card's resolution status when it changed (#522).
    pub fn set_rosen_resolution_status(
        &mut self,
        project: u32,
        identifier: u32,
        status: Option<RosenResolutionStatus>,
    ) -> Result<bool, StoreError> {
        let unchanged = self
            .project(project)
            .and_then(|item| item.threads.iter().find(|thread| thread.id == identifier))
            .is_none_or(|thread| thread.rosen_resolution_status == status);
        if unchanged {
            return Ok(false);
        }
        let mut next = self.state.clone();
        if let Some(thread) = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .and_then(|item| item.threads.iter_mut().find(|t| t.id == identifier))
        {
            thread.rosen_resolution_status = status;
        }
        self.store_snapshot(next)?;
        Ok(true)
    }

    /// Records or clears the latest background follow-up delivery error for a thread (#522).
    pub fn set_rosen_follow_up_error(
        &mut self,
        project: u32,
        identifier: u32,
        error: Option<String>,
    ) -> Result<bool, StoreError> {
        let unchanged = self
            .project(project)
            .and_then(|item| item.threads.iter().find(|thread| thread.id == identifier))
            .is_none_or(|thread| thread.rosen_follow_up_error == error);
        if unchanged {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            if let Some(thread) = item.threads.iter_mut().find(|t| t.id == identifier) {
                thread.rosen_follow_up_error = error;
                (true, ())
            } else {
                (false, ())
            }
        })?;
        Ok(true)
    }

    /// Drops any pending Conflict follow-up and its card status when Rosen has no open conflicts.
    pub fn clear_rosen_conflict_follow_up(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<bool, StoreError> {
        let needs_clear = self
            .project(project)
            .and_then(|item| item.threads.iter().find(|thread| thread.id == identifier))
            .is_some_and(|thread| {
                thread.rosen_resolution_status.is_some()
                    || thread
                        .rosen_follow_ups
                        .iter()
                        .any(|pending| matches!(pending, RosenFollowUp::Conflict { .. }))
            });
        if !needs_clear {
            return Ok(false);
        }
        self.edit_project_in_place(project, |item| {
            if let Some(thread) = item.threads.iter_mut().find(|t| t.id == identifier) {
                thread
                    .rosen_follow_ups
                    .retain(|pending| !matches!(pending, RosenFollowUp::Conflict { .. }));
                thread.rosen_resolution_status = None;
                (true, ())
            } else {
                (false, ())
            }
        })?;
        Ok(true)
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
        self.edit_project_in_place(project, |item| {
            for thread in &mut item.threads {
                if let Some((_, view)) = thread.rosen_change.as_ref().and_then(|id| views.get(id)) {
                    if view.conflicts.is_empty()
                        && !thread
                            .rosen_follow_ups
                            .iter()
                            .any(|pending| matches!(pending, RosenFollowUp::Conflict { .. }))
                    {
                        thread.rosen_resolution_status = None;
                    }
                    thread.rosen_change_view = Some(view.clone());
                }
            }
            (true, ())
        })?;
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
        self.edit_project_in_place(project, |item| {
            if seq <= item.rosen_event_cursor {
                return (false, ());
            }
            item.rosen_event_cursor = seq;
            if let Some(thread) = item.threads.iter_mut().find(|thread| {
                thread.rosen_change.as_deref() == Some(change)
                    && thread.state != ThreadState::Merged
            }) {
                if seq <= item.rosen_replay_through && notice_handled(thread, message) {
                    return (true, ());
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
                    thread.rosen_resolution_status.get_or_insert_with(|| {
                        RosenResolutionStatus::new(DEFAULT_RESOLUTION_STATUS.to_owned(), false)
                    });
                    RosenFollowUp::Conflict {
                        message: message.to_owned(),
                        mark: thread.transcript.len(),
                    }
                };
                thread.rosen_follow_ups.push(notice);
            }
            (true, ())
        })?;
        Ok(())
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
            if replacing || thread.rosen_last_reverted_event > latest {
                thread.rosen_last_reverted_event = 0;
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
        Ok(self
            .edit_project_in_place(project, |item| {
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
                (changed, changed)
            })?
            .unwrap_or(false))
    }

    /// Rebases without regeneration need no saved action: the periodic pass reads the basis.
    /// Their cursor still advances so reconnecting never replays older conflict events.
    pub fn save_rosen_cursor(&mut self, project: u32, seq: u64) -> Result<(), StoreError> {
        if seq <= self.rosen_event_cursor(project) {
            return Ok(());
        }
        self.edit_project_in_place(project, |item| {
            if seq <= item.rosen_event_cursor {
                return (false, ());
            }
            item.rosen_event_cursor = seq;
            (true, ())
        })?;
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
        match progress {
            RosenProgress::Landed => {
                if let Some(commit) = evidence.commit {
                    for repair in &mut item.ci_repairs {
                        if repair.thread == identifier {
                            repair.landed_commit = Some(commit.to_owned());
                        }
                    }
                }
            }
            RosenProgress::Reverted => {
                for repair in &mut item.ci_repairs {
                    if repair.thread == identifier {
                        repair.landed_commit = None;
                    }
                }
            }
            _ => {}
        }
        crate::rosen_work::restore_thread(item, identifier);
        if !matches!(progress, RosenProgress::Named | RosenProgress::Reverted) {
            crate::rosen_work::apply(item, change, progress, evidence);
        }
        let thread = &mut item.threads[index];
        let replaying = seq != 0 && seq <= item.rosen_replay_through;
        let legacy_cursor = item.rosen_legacy_replay_cursor;
        if replaying
            && record_legacy_progress(
                thread,
                seq,
                legacy_cursor,
                (progress, message),
                named_commit,
                evidence.commit,
            )
        {
            self.store_snapshot(next)?;
            return Ok(None);
        }
        if already_applied(
            thread,
            seq,
            replaying,
            progress,
            named_commit,
            evidence.commit,
        ) {
            self.store_snapshot(next)?;
            return Ok(None);
        }
        match progress {
            RosenProgress::Named => {
                thread.rosen_last_named_commit = named_commit.map(str::to_owned);
                crate::rosen_work::apply(item, change, progress, evidence);
            }
            RosenProgress::Reverted => {
                thread.rosen_last_reverted_commit = evidence.commit.map(str::to_owned);
                thread.rosen_last_reverted_event = seq;
                crate::rosen_work::apply(item, change, progress, evidence);
            }
            _ => {}
        }
        let thread = &mut item.threads[index];
        if matches!(
            progress,
            RosenProgress::Named | RosenProgress::Landed | RosenProgress::Abandoned
        ) && thread.rosen_overlap_wait.take().is_some()
        {
            thread.dependency_wait = thread.rosen_wait_description();
        }
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
        if landed {
            item.record_land(crate::landed_today::local_day());
        }
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
        item.rosen_follow_up_error = None;
        if matches!(done, RosenFollowUp::Conflict { .. }) {
            item.rosen_resolution_status = None;
        }
        self.store_snapshot(next)
    }

    /// The newest Rosen event `project`'s threads have taken in; 0 before the first.
    pub fn rosen_event_cursor(&self, project: u32) -> u64 {
        self.project(project)
            .map_or(0, |project| project.rosen_event_cursor)
    }
}

/// Recognizes and backfills pre-upgrade named/reverted events at or before the legacy
/// checkpoint so a one-time upgrade rewind stays idempotent without suppressing newer events.
fn record_legacy_progress(
    thread: &mut ThreadItem,
    sequence: u64,
    legacy_cursor: u64,
    (progress, message): (RosenProgress, &str),
    named_commit: Option<&str>,
    reverted_commit: Option<&str>,
) -> bool {
    if sequence > legacy_cursor {
        return false;
    }
    match progress {
        // Before named commits were tracked, a fixing decision at an in-range
        // checkpoint is evidence that its earlier name already moved to Review.
        RosenProgress::Named
            if thread.review_stage == Some(ReviewStage::Fixing)
                && thread.rosen_last_named_commit.is_none() =>
        {
            thread.rosen_last_named_commit = named_commit.map(str::to_owned);
            true
        }
        // Before revert identities were tracked, an in-range revert whose notice is
        // already pending or in the transcript was applied before the upgrade (#523).
        RosenProgress::Reverted
            if thread.state != ThreadState::Merged
                && thread.rosen_last_reverted_commit.is_none()
                && thread.rosen_last_reverted_event == 0
                && notice_handled(thread, message) =>
        {
            thread.rosen_last_reverted_commit = reverted_commit.map(str::to_owned);
            thread.rosen_last_reverted_event = sequence;
            true
        }
        _ => false,
    }
}

fn already_applied(
    thread: &ThreadItem,
    sequence: u64,
    replaying: bool,
    progress: RosenProgress,
    named_commit: Option<&str>,
    reverted_commit: Option<&str>,
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
            revert_already_applied(thread, sequence, reverted_commit)
        }
        _ => false,
    }
}

fn revert_already_applied(thread: &ThreadItem, sequence: u64, culprit: Option<&str>) -> bool {
    match (culprit, thread.rosen_last_reverted_commit.as_deref()) {
        (Some(culprit), Some(recorded)) => culprit == recorded,
        _ => sequence != 0 && sequence == thread.rosen_last_reverted_event,
    }
}

fn drop_delivered_conflicts(thread: &mut ThreadItem) {
    let drop_flags: Vec<bool> = thread
        .rosen_follow_ups
        .iter()
        .map(|pending| {
            matches!(pending, RosenFollowUp::Conflict { .. }) && pending.delivered(thread)
        })
        .collect();
    let mut index = 0;
    thread.rosen_follow_ups.retain(|_| {
        let keep = !drop_flags[index];
        index += 1;
        keep
    });
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
            thread.rosen_resolution_status = None;
            thread.rosen_follow_up_error = None;
            RosenFollowUp::FinishLanded
        }
        RosenProgress::Abandoned => {
            thread.rosen_follow_ups.clear();
            thread.rosen_resolution_status = None;
            thread.rosen_follow_up_error = None;
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

    // Risk (#523): on a rewind after two land/revert cycles with identical check output,
    // matching the prior transcript message suppresses the second revert, leaving the
    // thread in Review with no Reopen follow-up. Replaying the same revert must stay a no-op.
    #[test]
    fn rewind_applies_distinct_revert_with_identical_notice_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-rosen-revert-id-{unique}"));
        let message = "Your Rosen change `abc` landed, then failed: check exited with code 1";
        let _seeded = seed_reverted_then_named(&directory, message)?;

        // Reload after naming the repair, then rewind past an offline second land and revert.
        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.resume_rosen_events(1, "db-a", 6)?, (0, 6, true));
        let second = revert_evidence("second land");
        assert_eq!(
            store.save_rosen_work_progress(
                1,
                6,
                "abc",
                (RosenProgress::Reverted, message),
                second
            )?,
            Some(4),
        );
        let (pending, generation) = assert_second_revert_reopened(&store, message);

        // Replaying the same second revert across reload and database replacement is a no-op.
        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.resume_rosen_events(1, "db-b", 6)?, (0, 6, true));
        assert_eq!(
            store.save_rosen_work_progress(
                1,
                1,
                "abc",
                (RosenProgress::Reverted, message),
                second
            )?,
            None,
        );
        assert_eq!(store.rosen_follow_ups(1), pending);
        assert_eq!(
            store.state.projects[0].threads[0].rosen_progress_generation,
            generation
        );

        verify_legacy_unbound_cursor_reverts(&directory, message)?;
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    type PendingFollowUp = (u32, Option<String>, Vec<RosenFollowUp>);

    fn assert_second_revert_reopened(
        store: &StateStore,
        message: &str,
    ) -> (Vec<PendingFollowUp>, u64) {
        let thread = &store.state.projects[0].threads[0];
        assert_eq!(thread.state, ThreadState::Review);
        assert_eq!(thread.review_stage, Some(ReviewStage::Fixing));
        assert_eq!(
            thread.rosen_last_reverted_commit.as_deref(),
            Some("second land")
        );
        assert_eq!(thread.rosen_last_reverted_event, 6);
        assert_eq!(
            store.state.projects[0].rosen_work_items[0].status,
            velocity_core::work_pool::WorkStatus::Reopened
        );
        let pending = store.rosen_follow_ups(1);
        let [(4, Some(_), steps)] = pending.as_slice() else {
            panic!("expected one reopen follow-up: {pending:?}");
        };
        assert_eq!(steps.len(), 1);
        assert!(
            matches!(&steps[0], RosenFollowUp::Reopen { message: text, .. } if text == message)
        );
        assert!(!store.rosen_follow_up_delivered(1, 4, &steps[0]));
        (pending, thread.rosen_progress_generation)
    }

    fn revert_evidence(culprit: &str) -> WorkEvidence<'_> {
        WorkEvidence {
            commit: Some(culprit),
            revert_reason: Some("check exited with code 1"),
        }
    }

    fn seed_reverted_then_named(
        directory: &std::path::Path,
        message: &str,
    ) -> Result<StateStore, StoreError> {
        let mut store = checkpoint_store(directory)?;
        store.save_rosen_work_progress(
            1,
            2,
            "abc",
            (RosenProgress::Landed, ""),
            WorkEvidence {
                commit: Some("first land"),
                revert_reason: None,
            },
        )?;
        store.save_rosen_work_progress(
            1,
            3,
            "abc",
            (RosenProgress::Reverted, message),
            revert_evidence("first land"),
        )?;
        for (thread, _, steps) in store.rosen_follow_ups(1) {
            for step in steps {
                store.finish_rosen_follow_up(1, thread, &step)?;
            }
        }
        let mut delivered = store.state.clone();
        delivered.projects[0].threads[0].transcript =
            Arc::new(vec![TranscriptBlock::UserMessage {
                text: message.into(),
                queued: false,
            }]);
        store.store_snapshot(delivered)?;
        store.save_rosen_named(1, 4, "abc", Some("second head"))?;
        Ok(store)
    }

    fn seed_legacy_unbound_review(
        directory: &std::path::Path,
        message: &str,
    ) -> Result<StateStore, StoreError> {
        let mut store = seed_reverted_then_named(directory, message)?;
        let mut legacy = store.state.clone();
        legacy.projects[0].rosen_event_server_identity = None;
        legacy.projects[0].rosen_replay_through = 0;
        legacy.projects[0].threads[0].rosen_last_reverted_commit = None;
        legacy.projects[0].threads[0].rosen_last_reverted_event = 0;
        store.store_snapshot(legacy)?;
        StateStore::load_or_default(directory)
    }

    fn verify_legacy_unbound_cursor_reverts(
        directory: &std::path::Path,
        message: &str,
    ) -> Result<(), StoreError> {
        // Rewind from pre-upgrade cursor 4 directly to a superseded-land second revert at 6.
        let mut missed = seed_legacy_unbound_review(&directory.join("legacy-missed"), message)?;
        assert_eq!(missed.resume_rosen_events(1, "db-legacy", 6)?, (0, 6, true));
        assert_eq!(
            missed.save_rosen_work_progress(
                1,
                6,
                "abc",
                (RosenProgress::Reverted, message),
                revert_evidence("second land"),
            )?,
            Some(4),
        );
        assert_second_revert_reopened(&missed, message);

        // Rewind from pre-upgrade cursor 4 with no newer revert skips seq 3 and backfills it.
        let mut same = seed_legacy_unbound_review(&directory.join("legacy-same"), message)?;
        assert_eq!(same.resume_rosen_events(1, "db-legacy", 4)?, (0, 4, true));
        assert_eq!(
            same.save_rosen_work_progress(
                1,
                3,
                "abc",
                (RosenProgress::Reverted, message),
                revert_evidence("first land"),
            )?,
            None,
        );
        assert!(same.rosen_follow_ups(1).is_empty());
        assert_eq!(
            same.state.projects[0].threads[0]
                .rosen_last_reverted_commit
                .as_deref(),
            Some("first land")
        );
        assert_eq!(
            same.state.projects[0].threads[0].rosen_last_reverted_event,
            3
        );
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

    // Risk (#522): conflict resolution wait status and background delivery errors must survive
    // reload, and a stale delivered Conflict follow-up must not drop an explicit Resolve click.
    #[test]
    fn conflict_resolution_status_and_follow_up_error_survive_reload()
    -> Result<(), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-resolution-status-{unique}"));
        let mut store = checkpoint_store(&directory)?;
        let prompt = "Resolve the stored conflicts in Rosen change abc.".to_owned();
        assert_eq!(
            store.queue_rosen_resolution(1, 4, "abc", prompt.clone(), 0)?,
            QueueResolution::Queued,
        );
        assert_eq!(
            store.queue_rosen_resolution(1, 4, "abc", prompt.clone(), 0)?,
            QueueResolution::AlreadyPending,
        );
        assert!(store.set_rosen_resolution_status(
            1,
            4,
            Some(RosenResolutionStatus::new(
                "Can't resolve here · workspace missing".into(),
                true,
            )),
        )?);
        assert!(store.set_rosen_follow_up_error(
            1,
            4,
            Some("Could not deliver Rosen conflict resolution: workspace missing".into()),
        )?);

        let mut loaded = StateStore::load_or_default(&directory)?;
        let thread = &loaded.state.projects[0].threads[0];
        assert_eq!(
            thread.rosen_resolution_status,
            Some(RosenResolutionStatus::new(
                "Can't resolve here · workspace missing".into(),
                true,
            ))
        );
        assert_eq!(
            thread.rosen_follow_up_error.as_deref(),
            Some("Could not deliver Rosen conflict resolution: workspace missing")
        );

        // Simulate an earlier identical prompt already in the saved transcript with a stale mark:
        // a new explicit Resolve click must replace it with a fresh mark above the transcript.
        loaded.update_thread(1, 4, |item| {
            item.transcript = std::sync::Arc::new(vec![TranscriptBlock::UserMessage {
                text: prompt.clone(),
                queued: false,
            }]);
        })?;
        assert_eq!(
            loaded.queue_rosen_resolution(1, 4, "abc", prompt.clone(), 1)?,
            QueueResolution::Queued,
        );
        let follow_up = loaded.state.projects[0].threads[0].rosen_follow_ups[0].clone();
        assert!(!loaded.rosen_follow_up_delivered(1, 4, &follow_up));

        loaded.finish_rosen_follow_up(1, 4, &follow_up)?;
        let thread = &loaded.state.projects[0].threads[0];
        assert!(thread.rosen_resolution_status.is_none());
        assert!(thread.rosen_follow_up_error.is_none());
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
