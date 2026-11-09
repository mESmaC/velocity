//! Thread records for threads Velocity starts and runs (SPEC.md §2.1, §2.2, F4, F5).
//! Each change is written to `state.json` before the main window sees it.

use thiserror::Error;

use crate::{
    EffortLevel, ProjectItem, ProviderKind, SpeedMode, StateStore, StoreError, TaskState,
    ThreadItem,
};

#[derive(Debug, Error)]
pub enum ThreadError {
    #[error("Project {0} is not registered")]
    UnknownProject(u32),
    #[error("Thread {thread} is not in project {project}")]
    UnknownThread { project: u32, thread: u32 },
    #[error("Issue #{number} already belongs to thread {thread}")]
    IssueAssigned { number: u32, thread: u32 },
    #[error(transparent)]
    Persistence(#[from] StoreError),
}

/// What a new build thread starts with (F4): its title, branch and fixed worktree path (F5),
/// and the provider settings it runs on (F15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewThread {
    pub title: String,
    pub branch_name: String,
    pub worktree_path: String,
    pub provider: ProviderKind,
    pub model_name: String,
    pub model_supports_fast: bool,
    pub effort: EffortLevel,
    pub preferred_speed: SpeedMode,
}

impl NewThread {
    fn into_record(self, identifier: u32, issue: Option<crate::issues::ThreadIssue>) -> ThreadItem {
        let mut record = ThreadItem::new(identifier, self.title);
        record.branch_name = self.branch_name;
        record.worktree_path = Some(self.worktree_path);
        record.provider = self.provider;
        record.model_name = self.model_name;
        record.effort = self.effort;
        record.preferred_speed = self.preferred_speed;
        record.effective_speed = self.preferred_speed;
        record.model_supports_fast = self.model_supports_fast;
        record.issue = issue;
        record.normalize()
    }

    pub fn new(
        title: String,
        branch_name: String,
        worktree_path: String,
        settings: &crate::Settings,
    ) -> Self {
        Self {
            title,
            branch_name,
            worktree_path,
            provider: settings.providers.default_provider,
            model_name: settings
                .providers
                .default_model
                .clone()
                .unwrap_or_else(|| "agent default".into()),
            model_supports_fast: false,
            effort: settings.providers.default_effort,
            preferred_speed: settings.providers.default_speed,
        }
    }
}

impl StateStore {
    pub fn active_project(&self) -> Option<&ProjectItem> {
        crate::project_layout::active_project(&self.state)
    }

    pub fn project(&self, project: u32) -> Option<&ProjectItem> {
        self.state.projects.iter().find(|item| item.id == project)
    }

    /// Records a `working` thread and makes it the project's active thread.
    pub fn add_thread(&mut self, project: u32, thread: NewThread) -> Result<u32, ThreadError> {
        self.add_linked_thread(project, thread, None)
    }

    pub fn add_issue_thread(
        &mut self,
        project: u32,
        thread: NewThread,
        issue: crate::issues::ThreadIssue,
    ) -> Result<u32, ThreadError> {
        self.add_linked_thread(project, thread, Some(issue))
    }

    fn add_linked_thread(
        &mut self,
        project: u32,
        thread: NewThread,
        issue: Option<crate::issues::ThreadIssue>,
    ) -> Result<u32, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if let Some(issue) = &issue
            && let Some(existing) = crate::issues::issue_thread(item, issue)
        {
            return Err(ThreadError::IssueAssigned {
                number: issue.number,
                thread: existing.id,
            });
        }
        let identifier = item
            .threads
            .iter()
            .map(|existing| existing.id)
            .max()
            .map_or(1, |highest| highest.saturating_add(1));
        let record = thread.into_record(identifier, issue);
        // Newest first, as the reference's sidebar lists new threads at the top of their group.
        item.threads.insert(0, record);
        item.active_thread_id = Some(identifier);
        self.commit(next)?;
        Ok(identifier)
    }

    fn commit(&mut self, mut next: crate::PersistedState) -> Result<(), StoreError> {
        next.revision = self.state.revision.wrapping_add(1);
        crate::write_atomic_snapshot(&self.directory, &next)?;
        self.disk_revision = next.revision;
        self.state = next;
        Ok(())
    }

    /// Applies `change` to one thread and saves it when anything changed.
    pub fn update_thread(
        &mut self,
        project: u32,
        thread: u32,
        change: impl FnOnce(&mut ThreadItem),
    ) -> Result<bool, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?
            .threads
            .iter_mut()
            .find(|item| item.id == thread)
            .ok_or(ThreadError::UnknownThread { project, thread })?;
        let before = item.clone();
        change(item);
        if *item == before {
            return Ok(false);
        }
        self.commit(next)?;
        Ok(true)
    }

    /// Removes a deleted thread's record once its undo window has passed (F12). Tasks it
    /// was working on go back to todo for someone else to pick up, and it stops being the
    /// project's active thread. Returns the removed record.
    pub fn remove_thread(&mut self, project: u32, thread: u32) -> Result<ThreadItem, ThreadError> {
        let mut next = self.state.clone();
        let item = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        let position = item
            .threads
            .iter()
            .position(|record| record.id == thread)
            .ok_or(ThreadError::UnknownThread { project, thread })?;
        let removed = item.threads.remove(position);
        for task in &mut item.tasks {
            if task.assigned_thread_id == Some(thread) || removed.task_ids.contains(&task.id) {
                task.assigned_thread_id = None;
                if task.state == TaskState::Now {
                    task.state = TaskState::Todo;
                }
            }
        }
        if item.active_thread_id == Some(thread) {
            item.active_thread_id = None;
        }
        self.commit(next)?;
        Ok(removed)
    }
}
