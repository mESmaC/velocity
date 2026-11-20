//! Work ownership and lifecycle saved with the Rosen event cursor.

use velocity_core::thread_view_model::RosenChangeStage;
use velocity_core::work_pool::{WorkItem, WorkKey, WorkStatus};

use crate::issues::ThreadIssue;
use crate::rosen::RosenProgress;
use crate::threads::{ThreadError, thread_start_timestamp};
use crate::{ProjectItem, StateStore, StoreError, TaskState, ThreadItem, ThreadState};

/// Commit and diagnostic evidence carried by a Rosen land or revert event.
/// `commit` holds the landed commit for `Landed` and the reverted culprit commit for `Reverted`.
#[derive(Clone, Copy, Default)]
pub struct WorkEvidence<'a> {
    pub commit: Option<&'a str>,
    pub revert_reason: Option<&'a str>,
}

impl ProjectItem {
    pub fn rosen_work_item(&self, key: &WorkKey) -> Option<&WorkItem> {
        self.rosen_work_items.iter().find(|item| &item.key == key)
    }

    pub fn rosen_issue_completed(&self, issue: &ThreadIssue) -> bool {
        let key = WorkKey::issue(&issue.repository, issue.number);
        self.rosen_work_item(&key)
            .map(|item| item.status.completed())
            .unwrap_or_else(|| {
                self.completed_goal_issues
                    .iter()
                    .any(|completed| same_issue(completed, issue))
                    || self.threads.iter().any(|thread| {
                        thread.rosen_change.is_some()
                            && thread.state == ThreadState::Merged
                            && thread
                                .issue
                                .as_ref()
                                .is_some_and(|assigned| same_issue(assigned, issue))
                    })
            })
    }

    pub fn rosen_source_items(&self, identifier: u32) -> Vec<WorkKey> {
        self.threads
            .iter()
            .find(|thread| thread.id == identifier)
            .map(|thread| {
                sources(self, thread)
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn same_issue(left: &ThreadIssue, right: &ThreadIssue) -> bool {
    left.number == right.number && left.repository.eq_ignore_ascii_case(&right.repository)
}

fn sources(project: &ProjectItem, thread: &ThreadItem) -> Vec<(WorkKey, String)> {
    let mut items: Vec<_> = project
        .tasks
        .iter()
        .filter(|task| {
            task.assigned_thread_id == Some(thread.id)
                || (task.assigned_thread_id.is_none() && thread.task_ids.contains(&task.id))
        })
        .map(|task| {
            (
                WorkKey::Task {
                    identifier: task.id,
                },
                task.text.clone(),
            )
        })
        .collect();
    if let Some(issue) = &thread.issue {
        items.push((
            WorkKey::issue(&issue.repository, issue.number),
            thread.title.clone(),
        ));
    }
    items
}

fn track(project: &mut ProjectItem, identifier: u32, at: Option<&str>, restore: bool) {
    let Some(thread) = project
        .threads
        .iter()
        .find(|thread| thread.id == identifier)
    else {
        return;
    };
    if (thread.rosen_change.is_none() && project.version_control != crate::VersionControl::Rosen)
        || thread.supervision.released
        || thread.state == ThreadState::Done
        || (thread.state == ThreadState::Merged && thread.rosen_change.is_none())
    {
        return;
    }
    for (key, title) in sources(project, thread) {
        let current = project
            .rosen_work_items
            .iter_mut()
            .find(|item| item.key == key);
        match current {
            Some(item) if item.thread_identifier == Some(identifier) => {
                if item.change != thread.rosen_change {
                    item.change.clone_from(&thread.rosen_change);
                    item.status = WorkStatus::Claimed;
                    item.updated_at = at.map(str::to_owned);
                    item.landed_at = None;
                    item.green_at = None;
                    item.landed_commit = None;
                } else if restore
                    && item.status == WorkStatus::Landed
                    && item.landed_at.is_none()
                    && item.landed_commit.is_none()
                {
                    item.landed_commit = saved_land(thread);
                }
            }
            Some(item) if !restore && !item.status.completed() => {
                *item = claimed(key, title, thread, at);
            }
            Some(_) => {}
            None => {
                let mut item = claimed(key, title, thread, at);
                if restore {
                    restore_status(&mut item, thread);
                }
                project.rosen_work_items.push(item);
            }
        }
    }
}

fn claimed(key: WorkKey, title: String, thread: &ThreadItem, at: Option<&str>) -> WorkItem {
    WorkItem {
        key,
        title,
        thread_identifier: Some(thread.id),
        change: thread.rosen_change.clone(),
        status: WorkStatus::Claimed,
        claimed_at: at.map(str::to_owned),
        updated_at: at.map(str::to_owned),
        landed_at: None,
        green_at: None,
        reverted_at: None,
        landed_commit: None,
        revert_reason: None,
    }
}

fn restore_status(item: &mut WorkItem, thread: &ThreadItem) {
    let view = thread
        .rosen_change_view
        .as_ref()
        .filter(|view| Some(&view.identifier) == item.change.as_ref());
    if thread.state == ThreadState::Merged {
        item.status = WorkStatus::Landed;
        item.landed_commit = saved_land(thread);
    } else if view.is_some_and(|view| view.stage == RosenChangeStage::Reopened)
        || thread.rosen_issue.was_reverted()
    {
        item.status = WorkStatus::Reopened;
    } else if item.change.is_some()
        && (thread.review_stage.is_some() || view.is_some_and(|view| view.name.is_some()))
    {
        item.status = WorkStatus::InChange;
    }
}

fn saved_land(thread: &ThreadItem) -> Option<String> {
    thread
        .rosen_issue
        .landed_commit()
        .map(str::to_owned)
        .or_else(|| {
            let view = thread.rosen_change_view.as_ref()?;
            if thread.rosen_change.as_ref() != Some(&view.identifier) {
                return None;
            }
            let land = view.lands.last().filter(|land| land.revert.is_none())?;
            Some(land.commit.clone())
        })
}

pub(crate) fn track_claim(project: &mut ProjectItem, identifier: u32) {
    track(project, identifier, Some(&thread_start_timestamp()), false);
}

pub(crate) fn restore_thread(project: &mut ProjectItem, identifier: u32) {
    track(project, identifier, None, true);
}

/// Closing/deleting history releases active work and retains landed completion.
pub(crate) fn remove_thread(project: &mut ProjectItem, identifier: u32) {
    project.rosen_work_items.retain_mut(|item| {
        if item.thread_identifier != Some(identifier) {
            return true;
        }
        item.thread_identifier = None;
        item.status.completed()
    });
}

pub(crate) fn apply(
    project: &mut ProjectItem,
    change: &str,
    progress: RosenProgress,
    evidence: WorkEvidence<'_>,
) {
    if progress == RosenProgress::ChangesRequested {
        return;
    }
    let at = thread_start_timestamp();
    if progress == RosenProgress::Abandoned {
        project.rosen_work_items.retain(|item| {
            if item.change.as_deref() != Some(change) || item.status.completed() {
                return true;
            }
            if let WorkKey::Issue { repository, number } = &item.key {
                // Empty dispatch already retired its projection and keeps its no-change hold.
                project.continuous_dispatch.attempts.retain(|attempt| {
                    attempt.number != *number
                        || !attempt.repository.eq_ignore_ascii_case(repository)
                });
            }
            false
        });
        return;
    }
    for item in &mut project.rosen_work_items {
        if item.change.as_deref() != Some(change) {
            continue;
        }
        // After a cursor rewind, Rosen can skip a superseded revert and replay only
        // the latest land. Its commit distinguishes that land from the saved one.
        let replaced_land = progress == RosenProgress::Landed
            && evidence
                .commit
                .zip(item.landed_commit.as_deref())
                .is_some_and(|(observed, saved)| observed != saved);
        let status = match progress {
            RosenProgress::Named
                if matches!(item.status, WorkStatus::Claimed | WorkStatus::Reopened) =>
            {
                WorkStatus::InChange
            }
            RosenProgress::Landed if !item.status.completed() || replaced_land => {
                WorkStatus::Landed
            }
            RosenProgress::Reverted if item.status != WorkStatus::Reopened => WorkStatus::Reopened,
            _ => item.status,
        };
        let new_land = progress == RosenProgress::Landed
            && (status != item.status
                || replaced_land
                || (item.landed_commit.is_none() && evidence.commit.is_some()));
        if new_land {
            if replaced_land {
                item.landed_at = None;
                item.green_at = None;
            }
            item.landed_at.get_or_insert_with(|| at.clone());
            item.landed_commit = evidence.commit.map(str::to_owned);
        }
        if status == item.status && !replaced_land {
            continue;
        }
        item.status = status;
        item.updated_at = Some(at.clone());
        if status == WorkStatus::Reopened {
            reopen(
                item,
                &mut project.tasks,
                &mut project.completed_goal_issues,
                &at,
                evidence.revert_reason,
            );
        }
    }
}

fn reopen(
    item: &mut WorkItem,
    tasks: &mut [crate::TaskItem],
    completed_issues: &mut Vec<ThreadIssue>,
    at: &str,
    reason: Option<&str>,
) {
    item.landed_at = None;
    item.green_at = None;
    item.landed_commit = None;
    item.reverted_at = Some(at.to_owned());
    item.revert_reason = reason.map(str::to_owned);
    match &item.key {
        WorkKey::Issue { repository, number } => {
            // A deleted completing thread can no longer clear the legacy goal receipt.
            completed_issues.retain(|issue| {
                issue.number != *number || !issue.repository.eq_ignore_ascii_case(repository)
            });
        }
        WorkKey::Task { identifier } if item.thread_identifier.is_none() => {
            if let Some(task) = tasks.iter_mut().find(|task| task.id == *identifier)
                && task.state == TaskState::Done
            {
                task.state = TaskState::Todo;
            }
        }
        _ => {}
    }
}

impl StateStore {
    pub fn update_rosen_work_thread(
        &mut self,
        project: u32,
        thread: u32,
        update: impl FnOnce(&mut ThreadItem),
    ) -> Result<bool, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let record = item
            .threads
            .iter_mut()
            .find(|item| item.id == thread)
            .ok_or(ThreadError::UnknownThread { project, thread })?;
        let before = record.clone();
        update(record);
        let changed = *record != before;
        let before = item.rosen_work_items.clone();
        track_claim(item, thread);
        if changed || item.rosen_work_items != before {
            self.store_snapshot(next)?;
        }
        Ok(changed)
    }

    /// Seed records saved before the projection existed. The existing cursor remains valid.
    pub fn restore_rosen_work(&mut self, project: u32) -> Result<bool, StoreError> {
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(false);
        };
        let before = item.rosen_work_items.clone();
        let threads: Vec<_> = item.threads.iter().map(|thread| thread.id).collect();
        for thread in threads {
            track(item, thread, None, true);
        }
        if item.rosen_work_items == before {
            return Ok(false);
        }
        self.store_snapshot(next)?;
        Ok(true)
    }

    pub fn confirm_rosen_work_build(
        &mut self,
        project: u32,
        sequence: u64,
        covered: &[String],
    ) -> Result<bool, StoreError> {
        let mut next = self.state.clone();
        let Some(item) = next.projects.iter_mut().find(|item| item.id == project) else {
            return Ok(false);
        };
        if sequence != 0 && sequence <= item.rosen_event_cursor {
            return Ok(false);
        }
        let mut changed = false;
        let at = thread_start_timestamp();
        for item in &mut item.rosen_work_items {
            if item.status == WorkStatus::Landed
                && item
                    .landed_commit
                    .as_ref()
                    .is_some_and(|commit| covered.contains(commit))
            {
                item.status = WorkStatus::Green;
                item.updated_at = Some(at.clone());
                item.green_at = Some(at.clone());
                changed = true;
            }
        }
        if changed || sequence > item.rosen_event_cursor {
            item.rosen_event_cursor = item.rosen_event_cursor.max(sequence);
            self.store_snapshot(next)?;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TaskAttribution, TaskItem, VersionControl};
    use std::path::{Path, PathBuf};

    #[test]
    fn task_and_issue_follow_the_same_land_and_revert_across_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        // Saving a cursor without the corresponding work transition would lose the item
        // state permanently on reconnect. A test-only green event carries no build coverage.
        let (directory, mut store) = fixture()?;
        assert_items(&store, WorkStatus::Claimed, "original", Some(4));
        event(&mut store, 1, "original", RosenProgress::Named, None)?;
        assert_items(&store, WorkStatus::InChange, "original", Some(4));
        event(
            &mut store,
            2,
            "original",
            RosenProgress::Landed,
            Some("first"),
        )?;
        let mut store = StateStore::load_or_default(&directory)?;
        assert_eq!(store.rosen_event_cursor(1), 2);
        assert_items(&store, WorkStatus::Landed, "original", Some(4));
        assert!(!store.confirm_rosen_work_build(1, 3, &[])?);
        assert_items(&store, WorkStatus::Landed, "original", Some(4));
        assert!(store.confirm_rosen_work_build(1, 4, &["first".into()])?);
        assert_items(&store, WorkStatus::Green, "original", Some(4));
        event(&mut store, 5, "original", RosenProgress::Reverted, None)?;
        let mut store = StateStore::load_or_default(&directory)?;
        event(
            &mut store,
            2,
            "original",
            RosenProgress::Landed,
            Some("first"),
        )?;
        assert!(!store.confirm_rosen_work_build(1, 4, &["first".into()])?);
        assert_eq!(store.rosen_event_cursor(1), 5);
        assert_items(&store, WorkStatus::Reopened, "original", Some(4));
        assert_eq!(store.state.projects[0].tasks[0].state, TaskState::Now);
        event(&mut store, 6, "original", RosenProgress::Named, None)?;
        event(
            &mut store,
            7,
            "original",
            RosenProgress::Landed,
            Some("second"),
        )?;
        assert!(!store.confirm_rosen_work_build(1, 8, &["first".into()])?);
        assert!(store.confirm_rosen_work_build(1, 9, &["second".into()])?);
        store.remove_thread(1, 4)?;
        let store = StateStore::load_or_default(&directory)?;
        assert_items(&store, WorkStatus::Green, "original", None);
        assert!(
            store.state.projects[0]
                .rosen_issue_completed(&ThreadIssue::new("Owner/Repo".into(), 7))
        );
        assert_eq!(store.state.projects[0].tasks[0].state, TaskState::Done);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn abandonment_only_releases_the_current_change() -> Result<(), Box<dyn std::error::Error>> {
        // A predecessor can be abandoned after the replacement reaches disk.
        let (directory, mut store) = fixture()?;
        event(&mut store, 1, "original", RosenProgress::Named, None)?;
        store.update_rosen_work_thread(1, 4, |thread| {
            thread.rosen_change = Some("stacked".into());
        })?;
        let mut store = StateStore::load_or_default(&directory)?;
        event(&mut store, 2, "original", RosenProgress::Abandoned, None)?;
        assert_items(&store, WorkStatus::Claimed, "stacked", Some(4));
        event(&mut store, 3, "stacked", RosenProgress::Abandoned, None)?;
        let store = StateStore::load_or_default(&directory)?;
        let project = &store.state.projects[0];
        assert!(project.rosen_work_items.is_empty());
        assert!(
            !project
                .continuous_dispatch
                .attempted("owner/repo", 7, "unchanged")
        );
        assert_eq!(project.tasks[0].state, TaskState::Todo);
        assert!(project.tasks[0].assigned_thread_id.is_none());
        assert!(!project.rosen_issue_completed(&ThreadIssue::new("owner/repo".into(), 7)));
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    #[test]
    fn recovered_cursor_takes_the_latest_land_commit() -> Result<(), Box<dyn std::error::Error>> {
        // After a database rebuild, the latest land can supersede the older revert.
        // The latest land must use its own build coverage.
        let (directory, mut store) = fixture()?;
        store.resume_rosen_events(1, "before rebuild", 0)?;
        event(
            &mut store,
            1,
            "original",
            RosenProgress::Landed,
            Some("first"),
        )?;
        assert!(store.confirm_rosen_work_build(1, 2, &["first".into()])?);
        let mut store = StateStore::load_or_default(&directory)?;
        store.resume_rosen_events(1, "after rebuild", 1)?;
        event(
            &mut store,
            1,
            "original",
            RosenProgress::Landed,
            Some("second"),
        )?;
        assert_items(&store, WorkStatus::Landed, "original", Some(4));
        assert!(!store.confirm_rosen_work_build(1, 2, &["first".into()])?);
        assert!(store.confirm_rosen_work_build(1, 3, &["second".into()])?);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn fixture() -> Result<(PathBuf, StateStore), Box<dyn std::error::Error>> {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-rosen-work-{unique}"));
        Ok((directory.clone(), seed(&directory)?))
    }

    fn seed(directory: &Path) -> Result<StateStore, Box<dyn std::error::Error>> {
        let mut store = StateStore::load_or_default(directory)?;
        let mut project = ProjectItem::new(1, "Project".into(), r"C:\project".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        project
            .continuous_dispatch
            .attempts
            .push(crate::continuous_dispatch::DispatchAttempt {
                repository: "Owner/Repo".into(),
                number: 7,
                updated_at: "unchanged".into(),
            });
        let mut thread = ThreadItem::new(4, "Fix issue".into());
        thread.state = ThreadState::Working;
        thread.issue = Some(ThreadIssue::new("Owner/Repo".into(), 7));
        project.threads.push(thread);
        for number in [1, 2] {
            project.tasks.push(TaskItem::new(
                number,
                format!("Task {number}"),
                TaskState::Todo,
                TaskAttribution::User,
            ));
            crate::swarms::claim_task(&mut project, number, 4)?;
        }
        let mut next = store.state.clone();
        next.projects.push(project);
        store.store_snapshot(next)?;
        store.update_rosen_work_thread(1, 4, |thread| {
            thread.rosen_change = Some("original".into());
        })?;
        Ok(store)
    }

    fn assert_items(store: &StateStore, status: WorkStatus, change: &str, thread: Option<u32>) {
        let items = &store.state.projects[0].rosen_work_items;
        assert_eq!(items.len(), 3);
        for key in [
            WorkKey::Task { identifier: 1 },
            WorkKey::Task { identifier: 2 },
            WorkKey::issue("owner/repo", 7),
        ] {
            assert!(items.iter().any(|item| {
                item.key == key
                    && item.status == status
                    && item.change.as_deref() == Some(change)
                    && item.thread_identifier == thread
                    && item.claimed_at.is_some()
            }));
        }
    }

    fn event(
        store: &mut StateStore,
        sequence: u64,
        change: &str,
        progress: RosenProgress,
        commit: Option<&str>,
    ) -> Result<(), StoreError> {
        let evidence = WorkEvidence {
            commit,
            revert_reason: Some("the build failed"),
        };
        store.save_rosen_work_progress(1, sequence, change, (progress, ""), evidence)?;
        Ok(())
    }
}
