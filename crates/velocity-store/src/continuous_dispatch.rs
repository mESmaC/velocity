//! Saved per-project continuous dispatch (F8).
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeMap;

use crate::issues::ThreadIssue;
use crate::{
    ProjectItem, StateStore, TaskItem, TaskState, ThreadState, VersionControl, threads::ThreadError,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ContinuousDispatch {
    #[serde(default)]
    pub slots: u32,
    #[serde(default)]
    pub paused: bool,
    /// Do not send an unchanged open issue to another agent after a no-change result or
    /// while GitHub still shows an issue whose change just landed.
    #[serde(default)]
    pub attempts: Vec<DispatchAttempt>,
    /// A task whose turn ended with no change waits for an edit or an explicit reopen.
    #[serde(default)]
    #[specta(skip)]
    pub(crate) task_attempts: BTreeMap<u32, String>,
    /// Empty threads whose stopped workspace and claims still need cleanup.
    #[serde(default)]
    pub cleanup: Vec<u32>,
    /// Issue reports must survive workspace cleanup, thread deletion and failed GitHub calls.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub no_change_reports: BTreeMap<u32, NoChangeReport>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[specta(skip)]
    pub issue_overlaps: BTreeMap<u32, IssueOverlap>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueOverlap {
    pub issue: ThreadIssue,
    pub blocker: u32,
    #[serde(default)]
    pub blocker_thread: Option<u32>,
    #[serde(default)]
    pub review_commit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoChangeReport {
    pub issue: crate::issues::ThreadIssue,
    pub change: String,
    pub nick: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DispatchAttempt {
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub number: u32,
    #[serde(default)]
    pub updated_at: String,
}

impl ContinuousDispatch {
    pub fn reporting_no_change(&self, repository: &str, number: u32) -> bool {
        self.no_change_reports.values().any(|report| {
            report.issue.number == number
                && report.issue.repository.eq_ignore_ascii_case(repository)
        })
    }

    pub fn attempted(&self, repository: &str, number: u32, updated_at: &str) -> bool {
        self.attempts.iter().any(|attempt| {
            attempt.repository.eq_ignore_ascii_case(repository)
                && attempt.number == number
                && attempt.updated_at == updated_at
        })
    }
}

pub fn busy_count(project: &ProjectItem) -> u32 {
    project
        .threads
        .iter()
        .filter(|thread| {
            thread.interview.is_none()
                && !thread.supervision.released
                && (matches!(thread.state, ThreadState::Working | ThreadState::Review)
                    || (thread.started_by_dispatch
                        && (project.continuous_dispatch.cleanup.contains(&thread.id)
                            || thread.rosen_follow_ups.iter().any(|follow_up| {
                                matches!(
                                    follow_up,
                                    crate::rosen::RosenFollowUp::FinishLanded
                                        | crate::rosen::RosenFollowUp::FinishAbandoned
                                )
                            }))))
        })
        .count() as u32
}

pub fn has_capacity(project: &ProjectItem) -> bool {
    project.version_control == VersionControl::Rosen
        && project.spec_locked
        && !project.continuous_dispatch.paused
        && busy_count(project) < project.continuous_dispatch.slots
}

pub fn available_task(project: &ProjectItem, task: &TaskItem) -> bool {
    task.state == TaskState::Todo
        && task.assigned_thread_id.is_none()
        && !project
            .rosen_work_item(&velocity_core::work_pool::WorkKey::Task {
                identifier: task.id,
            })
            .is_some_and(|item| item.status.completed())
        && project.continuous_dispatch.task_attempts.get(&task.id) != Some(&task.text)
}

/// Reset activity is not evidence that saved user work was read. Both the admission
/// check and the final saved transition must reject unread messages.
pub fn can_finish_empty_dispatch(thread: &crate::ThreadItem) -> bool {
    thread.interview.is_none()
        && thread.state == ThreadState::Working
        && thread.activity == crate::AgentActivity::Idle
        && !thread.startup_recovery_pending
        && !crate::handoff::note_for(thread).is_some_and(|note| note.pending)
        && !thread.transcript.iter().any(|block| {
            matches!(
                block,
                velocity_core::thread_view_model::TranscriptBlock::UserMessage { queued: true, .. }
            )
        })
}

impl StateStore {
    pub fn save_issue_overlaps(
        &mut self,
        project: u32,
        overlaps: BTreeMap<u32, IssueOverlap>,
    ) -> Result<(), ThreadError> {
        let item = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if item.continuous_dispatch.issue_overlaps == overlaps {
            return Ok(());
        }
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        item.continuous_dispatch.issue_overlaps = overlaps;
        self.commit(next)?;
        Ok(())
    }

    /// One durable reservation for the whole startup batch, before activity is reset.
    pub fn reserve_startup_recovery(
        &mut self,
        interrupted: &[(u32, u32)],
    ) -> Result<(), crate::StoreError> {
        let mut next = self.state.clone();
        for (project, identifier) in interrupted {
            if let Some(thread) = next
                .projects
                .iter_mut()
                .find(|item| item.id == *project)
                .and_then(|project| {
                    project
                        .threads
                        .iter_mut()
                        .find(|thread| thread.id == *identifier)
                })
            {
                thread.startup_recovery_pending = true;
            }
        }
        if next != self.state {
            self.commit(next)?;
        }
        Ok(())
    }

    pub fn set_continuous_dispatch(
        &mut self,
        project: u32,
        slots: u32,
        paused: bool,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if item.version_control != VersionControl::Rosen {
            return Err(ThreadError::InvalidLaunch(
                "Continuous dispatch needs a Rosen project".into(),
            ));
        }
        if item.continuous_dispatch.slots == slots && item.continuous_dispatch.paused == paused {
            return Ok(());
        }
        item.continuous_dispatch.slots = slots;
        item.continuous_dispatch.paused = paused;
        self.commit(next)?;
        Ok(())
    }

    /// Save ownership and the attempted issue version together, without stealing focus.
    pub fn record_continuous_dispatch(
        &mut self,
        project: u32,
        thread: crate::threads::NewThread,
        issue: crate::issues::ThreadIssue,
        updated_at: String,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if !has_capacity(item)
            || item
                .continuous_dispatch
                .attempted(&issue.repository, issue.number, &updated_at)
            || item
                .continuous_dispatch
                .reporting_no_change(&issue.repository, issue.number)
            || item
                .continuous_dispatch
                .issue_overlaps
                .get(&issue.number)
                .is_some_and(|wait| {
                    wait.issue
                        .repository
                        .eq_ignore_ascii_case(&issue.repository)
                })
        {
            return Err(ThreadError::InvalidLaunch(
                "Dispatch settings or capacity changed".into(),
            ));
        }
        let selected = item.active_thread_id;
        let attempt = DispatchAttempt {
            repository: issue.repository.clone(),
            number: issue.number,
            updated_at,
        };
        let identifier = crate::threads::insert_thread(&mut next, project, thread, Some(issue))?;
        if let Some(item) = next.projects.iter_mut().find(|item| item.id == project) {
            mark_dispatch(item, selected, identifier);
            item.continuous_dispatch.attempts.retain(|prior| {
                prior.number != attempt.number
                    || !prior.repository.eq_ignore_ascii_case(&attempt.repository)
            });
            item.continuous_dispatch.attempts.push(attempt);
        }
        self.commit(next)?;
        Ok(identifier)
    }

    /// Save the thread and its task ownership together before starting an ACP session.
    pub fn record_continuous_task(
        &mut self,
        project: u32,
        thread: crate::threads::NewThread,
        task: u32,
    ) -> Result<u32, ThreadError> {
        let item = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if !has_capacity(item) {
            return Err(ThreadError::InvalidLaunch(
                "Dispatch settings or capacity changed".into(),
            ));
        }
        if !item
            .tasks
            .iter()
            .any(|entry| entry.id == task && available_task(item, entry))
        {
            return Err(ThreadError::TaskUnavailable(task));
        }
        let selected = item.active_thread_id;
        let mut next = self.state.clone();
        let identifier = crate::threads::insert_thread(&mut next, project, thread, None)?;
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        crate::swarms::claim_task(item, task, identifier)?;
        mark_dispatch(item, selected, identifier);
        self.commit(next)?;
        Ok(identifier)
    }
}

fn mark_dispatch(project: &mut ProjectItem, selected: Option<u32>, thread: u32) {
    let previous = project
        .threads
        .iter()
        .find(|entry| {
            entry.started_by_dispatch
                && matches!(entry.state, ThreadState::Done | ThreadState::Merged)
                && entry.rosen_follow_ups.is_empty()
                && !project.continuous_dispatch.cleanup.contains(&entry.id)
                && !project
                    .threads
                    .iter()
                    .any(|later| later.previous_work_thread == Some(entry.id))
        })
        .map(|entry| entry.id);
    project.active_thread_id = match previous {
        Some(prior) if selected == Some(prior) => Some(thread),
        _ => selected,
    };
    if let Some(record) = project.threads.iter_mut().find(|entry| entry.id == thread) {
        record.started_by_dispatch = true;
        record.previous_work_thread = previous;
    }
}

fn no_change_reason(thread: &crate::ThreadItem) -> String {
    use velocity_core::thread_view_model::TranscriptBlock;

    thread
        .transcript
        .iter()
        .rev()
        .find_map(|block| match block {
            TranscriptBlock::AgentMessage { paragraphs } => {
                let message = paragraphs.join("\n\n");
                (!message.trim().is_empty()).then_some(message)
            }
            _ => None,
        })
        .unwrap_or_else(|| "The agent left no final message.".into())
}

impl StateStore {
    pub fn finish_empty_dispatch(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<(), ThreadError> {
        self.finish_empty_dispatch_with_nick(project, identifier, None)
    }

    pub fn finish_empty_dispatch_with_nick(
        &mut self,
        project: u32,
        identifier: u32,
        nick: Option<String>,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let thread = item
            .threads
            .iter_mut()
            .find(|thread| thread.id == identifier)
            .ok_or(ThreadError::UnknownThread {
                project,
                thread: identifier,
            })?;
        if !can_finish_empty_dispatch(thread) {
            return Err(ThreadError::InvalidLaunch(
                "The thread resumed or still has unread messages".into(),
            ));
        }
        thread.state = ThreadState::Done;
        let reason = no_change_reason(thread);
        thread.no_change_reason = Some(reason.clone());
        if thread.started_by_dispatch {
            if let (Some(issue), Some(change)) = (&thread.issue, &thread.rosen_change) {
                item.continuous_dispatch.no_change_reports.insert(
                    identifier,
                    NoChangeReport {
                        issue: issue.clone(),
                        change: change.clone(),
                        nick,
                        reason,
                    },
                );
            }
            for task in &mut item.tasks {
                if task.assigned_thread_id == Some(identifier) && task.state == TaskState::Now {
                    item.continuous_dispatch
                        .task_attempts
                        .insert(task.id, thread.title.clone());
                    thread.task_ids.retain(|listed| *listed != task.id);
                    task.assigned_thread_id = None;
                    task.state = TaskState::Todo;
                }
            }
        }
        if !item.continuous_dispatch.cleanup.contains(&identifier) {
            item.continuous_dispatch.cleanup.push(identifier);
        }
        crate::rosen_work::remove_thread(item, identifier);
        self.commit(next)?;
        Ok(())
    }

    pub fn finish_no_change_report(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let dispatch = &mut next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?
            .continuous_dispatch;
        if let Some(report) = dispatch.no_change_reports.remove(&identifier) {
            // The GitHub label now owns eligibility. Removing it reopens dispatch
            // even if GitHub gives the issue the same updated_at timestamp.
            dispatch.attempts.retain(|attempt| {
                attempt.number != report.issue.number
                    || !attempt
                        .repository
                        .eq_ignore_ascii_case(&report.issue.repository)
            });
            self.commit(next)?;
        }
        Ok(())
    }

    pub fn finish_dispatch_cleanup(
        &mut self,
        project: u32,
        identifier: u32,
    ) -> Result<(), ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        item.continuous_dispatch
            .cleanup
            .retain(|pending| *pending != identifier);
        self.commit(next)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentActivity, PersistedState, issues::ThreadIssue, threads::NewThread};

    // Risk: a restart must keep pause, issue ownership, attempts and pending cleanup,
    // or an empty result can dispatch the same issue again and strand its path claims.
    #[test]
    fn continuous_dispatch_survives_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let (directory, mut store) = legacy_store()?;
        store.set_continuous_dispatch(1, 2, false)?;
        let thread = NewThread::new(
            "Issue".into(),
            "vel/issue-42".into(),
            directory.join("agent").display().to_string(),
            &store.settings(),
        );
        let issue = ThreadIssue::new("owner/repo".into(), 42);
        let identifier = store.record_continuous_dispatch(
            1,
            thread.clone(),
            issue.clone(),
            "version-1".into(),
        )?;
        let reason = "Computer Use failed. I could not verify the issue.";
        store.update_thread(1, identifier, |thread| {
            thread.activity = AgentActivity::Idle;
            thread.rosen_change = Some("empty-change".into());
            std::sync::Arc::make_mut(&mut thread.transcript).extend([
                velocity_core::thread_view_model::TranscriptBlock::AgentMessage {
                    paragraphs: vec!["I will check the issue.".into()],
                },
                velocity_core::thread_view_model::TranscriptBlock::AgentMessage {
                    paragraphs: vec![reason.into()],
                },
            ]);
        })?;
        store.finish_empty_dispatch_with_nick(1, identifier, Some("maple".into()))?;
        store.set_continuous_dispatch(1, 2, true)?;
        drop(store);
        let mut loaded = StateStore::load_or_default(&directory)?;
        let project = loaded.project(1).ok_or("missing project")?;
        assert_eq!(
            (
                project.continuous_dispatch.slots,
                project.continuous_dispatch.paused
            ),
            (2, true)
        );
        assert_eq!(project.continuous_dispatch.cleanup, vec![identifier]);
        assert_eq!(project.threads[0].state, ThreadState::Done);
        assert_eq!(project.active_thread_id, None);
        assert_eq!(project.threads[0].issue.as_ref(), Some(&issue));
        assert_eq!(project.threads[0].no_change_reason.as_deref(), Some(reason));
        let report = &project.continuous_dispatch.no_change_reports[&identifier];
        assert_eq!(report.issue, issue);
        assert_eq!(report.change, "empty-change");
        assert_eq!(report.reason, reason);
        assert_eq!(report.nick.as_deref(), Some("maple"));
        loaded.set_continuous_dispatch(1, 2, false)?;
        assert!(
            loaded
                .record_continuous_dispatch(1, thread.clone(), issue.clone(), "version-2".into())
                .is_err()
        );
        loaded.finish_no_change_report(1, identifier)?;
        loaded.record_continuous_dispatch(1, thread, issue, "version-1".into())?;
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    fn legacy_store() -> Result<(std::path::PathBuf, StateStore), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "velocity-dispatch-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory)?;
        let mut project = ProjectItem::new(1, "dispatch".into(), directory.display().to_string());
        project.spec_locked = true;
        let mut state = PersistedState::default();
        state.projects.push(project);
        let mut legacy = serde_json::to_value(state)?;
        legacy["projects"][0]
            .as_object_mut()
            .ok_or("missing project")?
            .remove("continuous_dispatch");
        std::fs::write(directory.join("state.json"), serde_json::to_vec(&legacy)?)?;
        let store = StateStore::load_or_default(&directory)?;
        assert_eq!(
            store
                .project(1)
                .ok_or("missing project")?
                .continuous_dispatch
                .slots,
            0
        );
        Ok((directory, store))
    }
}
