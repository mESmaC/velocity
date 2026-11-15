//! Saved per-project continuous issue dispatch (F8).
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{ProjectItem, StateStore, ThreadState, VersionControl, threads::ThreadError};

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
    /// Empty threads whose stopped workspace and claims still need cleanup.
    #[serde(default)]
    pub cleanup: Vec<u32>,
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
                && matches!(thread.state, ThreadState::Working | ThreadState::Review)
        })
        .count() as u32
}

pub fn has_capacity(project: &ProjectItem) -> bool {
    project.version_control == VersionControl::Rosen
        && project.spec_locked
        && !project.continuous_dispatch.paused
        && busy_count(project) < project.continuous_dispatch.slots
}

/// Reset activity is not evidence that saved user work was read. Both the admission
/// check and the final saved transition must reject unread messages.
pub fn can_finish_empty_dispatch(thread: &crate::ThreadItem) -> bool {
    thread.interview.is_none()
        && thread.state == ThreadState::Working
        && thread.activity == crate::AgentActivity::Idle
        && !thread.startup_recovery_pending
        && !thread.transcript.iter().any(|block| {
            matches!(
                block,
                velocity_core::thread_view_model::TranscriptBlock::UserMessage { queued: true, .. }
            )
        })
}

impl StateStore {
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
            item.active_thread_id = selected;
            item.continuous_dispatch.attempts.retain(|prior| {
                prior.number != attempt.number
                    || !prior.repository.eq_ignore_ascii_case(&attempt.repository)
            });
            item.continuous_dispatch.attempts.push(attempt);
        }
        self.commit(next)?;
        Ok(identifier)
    }
}

impl StateStore {
    pub fn finish_empty_dispatch(
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
        if !item.continuous_dispatch.cleanup.contains(&identifier) {
            item.continuous_dispatch.cleanup.push(identifier);
        }
        self.commit(next)?;
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
        store.update_thread(1, identifier, |thread| {
            thread.activity = AgentActivity::Idle
        })?;
        store.finish_empty_dispatch(1, identifier)?;
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
        loaded.set_continuous_dispatch(1, 2, false)?;
        assert!(
            loaded
                .record_continuous_dispatch(1, thread, issue, "version-1".into())
                .is_err()
        );
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
